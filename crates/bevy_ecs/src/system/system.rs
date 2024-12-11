use bevy_utils::tracing::warn;
use core::fmt::Debug;
use derive_more::derive::{Display, Error};

use crate::{
    archetype::ArchetypeComponentId,
    component::{ComponentId, Tick},
    query::Access,
    schedule::InternedSystemSet,
    system::{input::SystemInput, RunnableSystemMeta, SystemIn},
    world::{unsafe_world_cell::UnsafeWorldCell, DeferredWorld, World},
};

use alloc::borrow::Cow;
use core::any::TypeId;

use super::IntoSystem;

/// An ECS system that can be added to a [`Schedule`](crate::schedule::Schedule)
///
/// Systems are functions with all arguments implementing
/// [`SystemParam`](crate::system::SystemParam).
///
/// Systems are added to an application using `App::add_systems(Update, my_system)`
/// or similar methods, and will generally run once per pass of the main loop.
///
/// Systems are executed in parallel, in opportunistic order; data access is managed automatically.
/// It's possible to specify explicit execution order between specific systems,
/// see [`IntoSystemConfigs`](crate::schedule::IntoSystemConfigs).
#[diagnostic::on_unimplemented(message = "`{Self}` is not a system", label = "invalid system")]
pub trait System: Send + Sync + 'static {
    /// The system's input.
    type In: SystemInput;
    /// The system's output.
    type Out;
    /// Returns the system's name.
    fn name(&self) -> Cow<'static, str>;
    /// Returns the [`TypeId`] of the underlying system type.
    #[inline]
    fn type_id(&self) -> TypeId {
        TypeId::of::<Self>()
    }

    /// Runs the system with the given input in the world. Unlike [`System::run`], this function
    /// can be called in parallel with other systems and may break Rust's aliasing rules
    /// if used incorrectly, making it unsafe to call.
    ///
    /// Unlike [`System::run`], this will not apply deferred parameters, which must be independently
    /// applied by calling [`System::apply_deferred`] at later point in time.
    ///
    /// # Safety
    ///
    /// - The caller must ensure that [`world`](UnsafeWorldCell) has permission to access any world data
    ///   registered in `archetype_component_access`. There must be no conflicting
    ///   simultaneous accesses while the system is running.
    /// - The method [`System::update_archetype_component_access`] must be called at some
    ///   point before this one, with the same exact [`World`]. If [`System::update_archetype_component_access`]
    ///   panics (or otherwise does not return for any reason), this method must not be called.
    unsafe fn run_unsafe(&mut self, input: SystemIn<'_, Self>, world: UnsafeWorldCell)
        -> Self::Out;

    /// Applies any [`Deferred`](crate::system::Deferred) system parameters (or other system buffers) of this system to the world.
    ///
    /// This is where [`Commands`](crate::system::Commands) get applied.
    fn apply_deferred(&mut self, world: &mut World);

    /// Enqueues any [`Deferred`](crate::system::Deferred) system parameters (or other system buffers)
    /// of this system into the world's command buffer.
    fn queue_deferred(&mut self, world: DeferredWorld);

    /// Validates that all parameters can be acquired and that system can run without panic.
    /// Built-in executors use this to prevent invalid systems from running.
    ///
    /// However calling and respecting [`System::validate_param_unsafe`] or it's safe variant
    /// is not a strict requirement, both [`System::run`] and [`System::run_unsafe`]
    /// should provide their own safety mechanism to prevent undefined behavior.
    ///
    /// This method has to be called directly before [`System::run_unsafe`] with no other (relevant)
    /// world mutations in between. Otherwise, while it won't lead to any undefined behavior,
    /// the validity of the param may change.
    ///
    /// # Safety
    ///
    /// - The caller must ensure that [`world`](UnsafeWorldCell) has permission to access any world data
    ///   registered in `archetype_component_access`. There must be no conflicting
    ///   simultaneous accesses while the system is running.
    /// - The method [`System::update_archetype_component_access`] must be called at some
    ///   point before this one, with the same exact [`World`]. If [`System::update_archetype_component_access`]
    ///   panics (or otherwise does not return for any reason), this method must not be called.
    unsafe fn validate_param_unsafe(&mut self, world: UnsafeWorldCell) -> bool;

    /// Initialize the system.
    fn initialize(&mut self, _world: &mut World) -> RunnableSystemMeta;

    /// Update the system's archetype component [`Access`].
    ///
    /// ## Note for implementors
    /// `world` may only be used to access metadata. This can be done in safe code
    /// via functions such as [`UnsafeWorldCell::archetypes`].
    fn update_archetype_component_access(
        &mut self,
        world: UnsafeWorldCell,
        runnable_system_meta: &mut RunnableSystemMeta,
    );

    /// Checks any [`Tick`]s stored on this system and wraps their value if they get too old.
    ///
    /// This method must be called periodically to ensure that change detection behaves correctly.
    /// When using bevy's default configuration, this will be called for you as needed.
    fn check_change_tick(&mut self, change_tick: Tick);

    /// Returns the system's default [system sets](crate::schedule::SystemSet).
    ///
    /// Each system will create a default system set that contains the system.
    fn default_system_sets(&self) -> Vec<InternedSystemSet> {
        Vec::new()
    }

    /// Gets the tick indicating the last time this system ran.
    fn get_last_run(&self) -> Tick;

    /// Overwrites the tick indicating the last time this system ran.
    ///
    /// # Warning
    /// This is a complex and error-prone operation, that can have unexpected consequences on any system relying on this code.
    /// However, it can be an essential escape hatch when, for example,
    /// you are trying to synchronize representations using change detection and need to avoid infinite recursion.
    fn set_last_run(&mut self, last_run: Tick);
}

pub struct RunnableSystem<S: ?Sized> {
    runnable_system_meta: RunnableSystemMeta,
    system: S,
}

impl<S: System + ?Sized> RunnableSystem<S> {
    pub fn new<M>(system: impl IntoSystem<S::In, S::Out, M, System = S>) -> Self
    where
        S: Sized,
    {
        let system = IntoSystem::into_system(system);
        let runnable_system_meta = RunnableSystemMeta::new();
        Self {
            system,
            runnable_system_meta,
        }
    }

    pub fn new_boxed<M>(system: impl IntoSystem<S::In, S::Out, M, System = S>) -> Box<Self>
    where
        S: Sized,
    {
        Box::new(Self::new(system))
    }

    pub fn name(&self) -> Cow<'static, str> {
        self.system.name()
    }

    pub fn system(&self) -> &S {
        &self.system
    }

    pub fn system_mut(&mut self) -> &mut S {
        &mut self.system
    }

    pub fn system_type_id(&self) -> TypeId {
        self.system.type_id()
    }

    pub fn update_archetype_component_access(&mut self, world: UnsafeWorldCell) {
        self.system
            .update_archetype_component_access(world, &mut self.runnable_system_meta);
    }

    pub fn archetype_component_access(&self) -> &Access<ArchetypeComponentId> {
        &self.runnable_system_meta.archetype_component_access
    }

    pub fn component_access(&self) -> &Access<ComponentId> {
        &self
            .runnable_system_meta
            .component_access_set
            .combined_access()
    }

    pub fn is_send(&self) -> bool {
        self.runnable_system_meta.is_send()
    }

    pub fn is_exclusive(&self) -> bool {
        self.runnable_system_meta.is_exclusive()
    }

    pub fn has_deferred(&self) -> bool {
        self.runnable_system_meta.has_deferred()
    }

    pub unsafe fn run_unsafe(&mut self, input: SystemIn<'_, S>, world: UnsafeWorldCell) -> S::Out {
        self.system.run_unsafe(input, world)
    }

    pub fn initialize(&mut self, world: &mut World) {
        self.runnable_system_meta = self.system.initialize(world);
    }

    // TODO: Note that this is for apply_deferred
    pub(crate) fn initialize_as_exclusive(&mut self) {
        self.runnable_system_meta.set_non_send();
        self.runnable_system_meta.set_is_exclusive();
    }

    pub fn run(&mut self, input: SystemIn<'_, S>, world: &mut World) -> S::Out {
        let world_cell = world.as_unsafe_world_cell();
        self.system
            .update_archetype_component_access(world_cell, &mut self.runnable_system_meta);
        // SAFETY:
        // - We have exclusive access to the entire world.
        // - `update_archetype_component_access` has been called.
        let ret = unsafe { self.system.run_unsafe(input, world_cell) };
        self.system.apply_deferred(world);
        ret
    }

    pub fn validate_param(&mut self, world: &World) -> bool {
        let world_cell = world.as_unsafe_world_cell_readonly();
        self.system
            .update_archetype_component_access(world_cell, &mut self.runnable_system_meta);
        // SAFETY:
        // - We have exclusive access to the entire world.
        // - `update_archetype_component_access` has been called.
        unsafe { self.system.validate_param_unsafe(world_cell) }
    }

    pub fn run_deferred(&mut self, input: SystemIn<'_, S>, world: UnsafeWorldCell) {
        self.system
            .update_archetype_component_access(world, &mut self.runnable_system_meta);
        unsafe {
            if self.system.validate_param_unsafe(world) {
                self.system.run_unsafe(input, world);
                self.system.queue_deferred(world.into_deferred());
            }
        }
    }
}

/// [`System`] types that do not modify the [`World`] when run.
/// This is implemented for any systems whose parameters all implement [`ReadOnlySystemParam`].
///
/// Note that systems which perform [deferred](System::apply_deferred) mutations (such as with [`Commands`])
/// may implement this trait.
///
/// [`ReadOnlySystemParam`]: crate::system::ReadOnlySystemParam
/// [`Commands`]: crate::system::Commands
///
/// # Safety
///
/// This must only be implemented for system types which do not mutate the `World`
/// when [`System::run_unsafe`] is called.
pub unsafe trait ReadOnlySystem: System {}

/// A convenience type alias for a boxed [`System`] trait object.
pub type BoxedSystem<In = (), Out = ()> = Box<dyn System<In = In, Out = Out>>;

pub type RunnableBoxedSystem<In = (), Out = ()> =
    Box<RunnableSystem<dyn System<In = In, Out = Out>>>;

pub(crate) fn check_system_change_tick(last_run: &mut Tick, this_run: Tick, system_name: &str) {
    if last_run.check_tick(this_run) {
        let age = this_run.relative_to(*last_run).get();
        warn!(
            "System '{system_name}' has not run for {age} ticks. \
            Changes older than {} ticks will not be detected.",
            Tick::MAX.get() - 1,
        );
    }
}

impl<In, Out> Debug for dyn System<In = In, Out = Out>
where
    In: SystemInput + 'static,
    Out: 'static,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("System")
            .field("name", &self.name())
            .finish_non_exhaustive()
    }
}

/// Trait used to run a system immediately on a [`World`].
///
/// # Warning
/// This function is not an efficient method of running systems and it's meant to be used as a utility
/// for testing and/or diagnostics.
///
/// Systems called through [`run_system_once`](RunSystemOnce::run_system_once) do not hold onto any state,
/// as they are created and destroyed every time [`run_system_once`](RunSystemOnce::run_system_once) is called.
/// Practically, this means that [`Local`](crate::system::Local) variables are
/// reset on every run and change detection does not work.
///
/// ```
/// # use bevy_ecs::prelude::*;
/// # use bevy_ecs::system::RunSystemOnce;
/// #[derive(Resource, Default)]
/// struct Counter(u8);
///
/// fn increment(mut counter: Local<Counter>) {
///    counter.0 += 1;
///    println!("{}", counter.0);
/// }
///
/// let mut world = World::default();
/// world.run_system_once(increment); // prints 1
/// world.run_system_once(increment); // still prints 1
/// ```
///
/// If you do need systems to hold onto state between runs, use [`World::run_system_cached`](World::run_system_cached)
/// or [`World::run_system`](World::run_system).
///
/// # Usage
/// Typically, to test a system, or to extract specific diagnostics information from a world,
/// you'd need a [`Schedule`](crate::schedule::Schedule) to run the system. This can create redundant boilerplate code
/// when writing tests or trying to quickly iterate on debug specific systems.
///
/// For these situations, this function can be useful because it allows you to execute a system
/// immediately with some custom input and retrieve its output without requiring the necessary boilerplate.
///
/// # Examples
///
/// ## Immediate Command Execution
///
/// This usage is helpful when trying to test systems or functions that operate on [`Commands`](crate::system::Commands):
/// ```
/// # use bevy_ecs::prelude::*;
/// # use bevy_ecs::system::RunSystemOnce;
/// let mut world = World::default();
/// let entity = world.run_system_once(|mut commands: Commands| {
///     commands.spawn_empty().id()
/// }).unwrap();
/// # assert!(world.get_entity(entity).is_ok());
/// ```
///
/// ## Immediate Queries
///
/// This usage is helpful when trying to run an arbitrary query on a world for testing or debugging purposes:
/// ```
/// # use bevy_ecs::prelude::*;
/// # use bevy_ecs::system::RunSystemOnce;
///
/// #[derive(Component)]
/// struct T(usize);
///
/// let mut world = World::default();
/// world.spawn(T(0));
/// world.spawn(T(1));
/// world.spawn(T(1));
/// let count = world.run_system_once(|query: Query<&T>| {
///     query.iter().filter(|t| t.0 == 1).count()
/// }).unwrap();
///
/// # assert_eq!(count, 2);
/// ```
///
/// Note that instead of closures you can also pass in regular functions as systems:
///
/// ```
/// # use bevy_ecs::prelude::*;
/// # use bevy_ecs::system::RunSystemOnce;
///
/// #[derive(Component)]
/// struct T(usize);
///
/// fn count(query: Query<&T>) -> usize {
///     query.iter().filter(|t| t.0 == 1).count()
/// }
///
/// let mut world = World::default();
/// world.spawn(T(0));
/// world.spawn(T(1));
/// world.spawn(T(1));
/// let count = world.run_system_once(count).unwrap();
///
/// # assert_eq!(count, 2);
/// ```
pub trait RunSystemOnce: Sized {
    /// Tries to run a system and apply its deferred parameters.
    fn run_system_once<T, Out, Marker>(self, system: T) -> Result<Out, RunSystemError>
    where
        T: IntoSystem<(), Out, Marker>,
    {
        self.run_system_once_with((), system)
    }

    /// Tries to run a system with given input and apply deferred parameters.
    fn run_system_once_with<T, In, Out, Marker>(
        self,
        input: SystemIn<'_, T::System>,
        system: T,
    ) -> Result<Out, RunSystemError>
    where
        T: IntoSystem<In, Out, Marker>,
        In: SystemInput;
}

impl RunSystemOnce for &mut World {
    fn run_system_once_with<T, In, Out, Marker>(
        self,
        input: SystemIn<'_, T::System>,
        system: T,
    ) -> Result<Out, RunSystemError>
    where
        T: IntoSystem<In, Out, Marker>,
        In: SystemInput,
    {
        let system: T::System = IntoSystem::into_system(system);
        let mut system = RunnableSystem::new(system);
        system.initialize(self);
        if system.validate_param(self) {
            Ok(system.run(input, self))
        } else {
            Err(RunSystemError::InvalidParams(system.name()))
        }
    }
}

/// Running system failed.
#[derive(Error, Display)]
pub enum RunSystemError {
    /// System could not be run due to parameters that failed validation.
    ///
    /// This can occur because the data required by the system was not present in the world.
    #[display("The data required by the system {_0:?} was not found in the world and the system did not run due to failed parameter validation.")]
    #[error(ignore)]
    InvalidParams(Cow<'static, str>),
}

impl Debug for RunSystemError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidParams(arg0) => f.debug_tuple("InvalidParams").field(arg0).finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate as bevy_ecs;
    use crate::prelude::*;

    #[test]
    fn run_system_once() {
        struct T(usize);

        impl Resource for T {}

        fn system(In(n): In<usize>, mut commands: Commands) -> usize {
            commands.insert_resource(T(n));
            n + 1
        }

        let mut world = World::default();
        let n = world.run_system_once_with(1, system).unwrap();
        assert_eq!(n, 2);
        assert_eq!(world.resource::<T>().0, 1);
    }

    #[derive(Resource, Default, PartialEq, Debug)]
    struct Counter(u8);

    #[allow(dead_code)]
    fn count_up(mut counter: ResMut<Counter>) {
        counter.0 += 1;
    }

    #[test]
    fn run_two_systems() {
        let mut world = World::new();
        world.init_resource::<Counter>();
        assert_eq!(*world.resource::<Counter>(), Counter(0));
        world.run_system_once(count_up).unwrap();
        assert_eq!(*world.resource::<Counter>(), Counter(1));
        world.run_system_once(count_up).unwrap();
        assert_eq!(*world.resource::<Counter>(), Counter(2));
    }

    #[allow(dead_code)]
    fn spawn_entity(mut commands: Commands) {
        commands.spawn_empty();
    }

    #[test]
    fn command_processing() {
        let mut world = World::new();
        assert_eq!(world.entities.len(), 0);
        world.run_system_once(spawn_entity).unwrap();
        assert_eq!(world.entities.len(), 1);
    }

    #[test]
    fn non_send_resources() {
        fn non_send_count_down(mut ns: NonSendMut<Counter>) {
            ns.0 -= 1;
        }

        let mut world = World::new();
        world.insert_non_send_resource(Counter(10));
        assert_eq!(*world.non_send_resource::<Counter>(), Counter(10));
        world.run_system_once(non_send_count_down).unwrap();
        assert_eq!(*world.non_send_resource::<Counter>(), Counter(9));
    }

    #[test]
    fn run_system_once_invalid_params() {
        struct T;
        impl Resource for T {}
        fn system(_: Res<T>) {}

        let mut world = World::default();
        // This fails because `T` has not been added to the world yet.
        let result = world.run_system_once(system);

        assert!(matches!(result, Err(RunSystemError::InvalidParams(_))));
    }
}

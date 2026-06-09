//! Contains the definition of the [`Command`] trait,
//! as well as the blanket implementation of the trait for closures.
//!
//! It also contains functions that return closures for use with
//! [`Commands`](crate::system::Commands).

use bevy_ptr::{deconstruct_moving_ptr, MovingPtr};
use bevy_utils::prelude::DebugName;

use crate::{
    bundle::{Bundle, InsertMode, NoBundleEffect},
    change_detection::MaybeLocation,
    entity::Entity,
    error::{BevyError, CommandOutput, ErrorContext, Result},
    event::Event,
    message::{Message, Messages},
    resource::Resource,
    schedule::ScheduleLabel,
    system::{IntoSystem, SystemId, SystemInput},
    world::{FromWorld, SpawnBatchIter, World},
};

/// A [`World`] mutation.
///
/// Should be used with [`Commands::queue`](crate::system::Commands::queue).
///
/// The `Out` generic parameter is the returned "output" of the command.
///
/// It is not usually necessary to implement this manually,
/// as it is implemented for closures that accept `&mut World`.
///
/// If the command captures a large amount of data,
/// you can use [`PtrCommand`] to avoid a copy.
///
/// # Usage
///
/// ```
/// # use bevy_ecs::prelude::*;
/// # use bevy_ecs::system::command::PtrCommand;
/// # use bevy_ptr::MovingPtr;
/// // Our world resource
/// #[derive(Resource, Default)]
/// struct Counter(u64);
///
/// // Our custom command
/// struct AddToCounter(u64);
///
/// impl Command for AddToCounter {
///     type Out = ();
///
///     fn apply(this: MovingPtr<Self>, world: &mut World) {
///         let mut counter = world.get_resource_or_insert_with(Counter::default);
///         counter.0 += this.0;
///     }
/// }
///
/// fn some_system(mut commands: Commands) {
///     // Using a named command type
///     commands.queue(AddToCounter(42));
///
///     // Using a closure
///     let add_to = 42;
///     commands.queue(move |world: &mut World| {
///         let mut counter = world.get_resource_or_insert_with(Counter::default);
///         counter.0 += add_to;
///     });
///
///     // Using `PtrCommand`
///     commands.queue(PtrCommand::new(add_to, |add_to, world| {
///         let mut counter = world.get_resource_or_insert_with(Counter::default);
///         counter.0 += *add_to;
///     }));
/// }
/// ```
pub trait Command: Sized + Send + 'static {
    /// The return type of [`apply`](Command::apply).
    type Out: CommandOutput;

    /// Applies this command, causing it to mutate the provided `world`.
    ///
    /// This method is used to define what a command "does" when it is ultimately applied.
    /// Because this method takes `self`, you can store data or settings on the type that implements this trait.
    /// This data is set by the system or other source of the command, and then ultimately read in this method.
    fn apply(this: MovingPtr<Self>, world: &mut World) -> Self::Out;

    /// Takes a [`Command`] that returns a Result and uses a given error handler function to convert it into
    /// a [`Command`] that internally handles an error if it occurs and returns `()`.
    #[inline]
    fn handle_error_with(
        self,
        error_handler: impl FnOnce(BevyError, ErrorContext) + Send + 'static,
    ) -> impl Command<Out = ()> {
        PtrCommand::new(self, move |this, world| {
            if let Some(error) = Self::apply(this, world).to_err() {
                error_handler(
                    error,
                    ErrorContext::Command {
                        name: DebugName::type_name::<Self>(),
                    },
                );
            }
        })
    }

    /// Takes a [`Command`] that returns a Result and uses the fallback error handler function to convert it into
    /// a [`Command`] that internally handles an error if it occurs and returns `()`.
    #[inline]
    fn handle_error(self) -> impl Command<Out = ()> {
        PtrCommand::new(self, |this, world| {
            if let Some(error) = Self::apply(this, world).to_err() {
                world.fallback_error_handler()(
                    error,
                    ErrorContext::Command {
                        name: DebugName::type_name::<Self>(),
                    },
                );
            }
        })
    }

    /// Takes a [`Command`] that returns a Result and ignores any error that occurs.
    #[inline]
    fn ignore_error(self) -> impl Command<Out = ()> {
        PtrCommand::new(self, |this, world| {
            let _ = Self::apply(this, world);
        })
    }
}

impl<F, Out> Command for F
where
    F: FnOnce(&mut World) -> Out + Send + 'static,
    Out: CommandOutput,
{
    type Out = Out;

    fn apply(this: MovingPtr<Self>, world: &mut World) -> Out {
        this.read()(world)
    }
}

/// A [`Command`] that stores its data directly
/// in the command buffer while executing.
///
/// This can be useful for commands with large data,
/// since it avoids the need to copy it to the stack.
pub struct PtrCommand<T, F> {
    data: T,
    func: F,
}

impl<T, F, Out> PtrCommand<T, F>
where
    T: Send + 'static,
    F: FnOnce(MovingPtr<T>, &mut World) -> Out + Send + 'static,
    Out: CommandOutput,
{
    /// Creates a new [`PtrCommand`].
    ///
    /// The `data` will be stored as part of the command and
    /// passed to `func` as a [`MovingPtr`] without being copied.
    pub fn new(data: T, func: F) -> Self {
        Self { data, func }
    }
}

impl<T, F, Out> Command for PtrCommand<T, F>
where
    T: Send + 'static,
    F: FnOnce(MovingPtr<T>, &mut World) -> Out + Send + 'static,
    Out: CommandOutput,
{
    type Out = Out;

    fn apply(this: MovingPtr<Self>, world: &mut World) -> Self::Out {
        deconstruct_moving_ptr!({
            let Self { func, data } = this;
        });
        func.read()(data, world)
    }
}

/// A [`Command`] that consumes an iterator of [`Bundles`](Bundle) to spawn a series of entities.
///
/// This is more efficient than spawning the entities individually.
#[track_caller]
pub fn spawn_batch<I>(bundles_iter: I) -> impl Command
where
    I: IntoIterator + Send + Sync + 'static,
    I::Item: Bundle<Effect: NoBundleEffect>,
{
    let caller = MaybeLocation::caller();
    move |world: &mut World| {
        SpawnBatchIter::new(world, bundles_iter.into_iter(), caller);
    }
}

/// A [`Command`] that consumes an iterator to add a series of [`Bundles`](Bundle) to a set of entities.
///
/// If any entities do not exist in the world, this command will return a
/// [`TryInsertBatchError`](crate::world::error::TryInsertBatchError).
///
/// This is more efficient than inserting the bundles individually.
#[track_caller]
pub fn insert_batch<I, B>(batch: I, insert_mode: InsertMode) -> impl Command
where
    I: IntoIterator<Item = (Entity, B)> + Send + Sync + 'static,
    B: Bundle<Effect: NoBundleEffect>,
{
    let caller = MaybeLocation::caller();
    move |world: &mut World| -> Result {
        world.try_insert_batch_with_caller(batch, insert_mode, caller)?;
        Ok(())
    }
}

/// A [`Command`] that inserts a [`Resource`] into the world using a value
/// created with the [`FromWorld`] trait.
#[track_caller]
pub fn init_resource<R: Resource + FromWorld>() -> impl Command {
    move |world: &mut World| {
        world.init_resource::<R>();
    }
}

/// A [`Command`] that inserts a [`Resource`] into the world.
#[track_caller]
pub fn insert_resource<R: Resource>(resource: R) -> impl Command {
    let caller = MaybeLocation::caller();
    move |world: &mut World| {
        world.insert_resource_with_caller(resource, caller);
    }
}

/// A [`Command`] that removes a [`Resource`] from the world.
pub fn remove_resource<R: Resource>() -> impl Command {
    move |world: &mut World| {
        world.remove_resource::<R>();
    }
}

/// A [`Command`] that runs the system corresponding to the given [`SystemId`].
pub fn run_system<O: 'static>(id: impl Into<SystemId<(), O>> + Send) -> impl Command {
    let id = id.into();
    move |world: &mut World| -> Result {
        world.run_system(id)?;
        Ok(())
    }
}

/// A [`Command`] that runs the system corresponding to the given [`SystemId`]
/// and provides the given input value.
pub fn run_system_with<I>(
    id: impl Into<SystemId<I>> + Send,
    input: I::Inner<'static>,
) -> impl Command
where
    I: SystemInput<Inner<'static>: Send> + 'static,
{
    let id = id.into();
    move |world: &mut World| -> Result {
        world.run_system_with(id, input)?;
        Ok(())
    }
}

/// A [`Command`] that runs the given system,
/// caching its [`SystemId`] in a [`CachedSystemId`](crate::system::CachedSystemId) resource.
pub fn run_system_cached<M, S>(system: S) -> impl Command
where
    M: 'static,
    S: IntoSystem<(), (), M> + Send + 'static,
{
    move |world: &mut World| -> Result {
        world.run_system_cached(system)?;
        Ok(())
    }
}

/// A [`Command`] that runs the given system with the given input value,
/// caching its [`SystemId`] in a [`CachedSystemId`](crate::system::CachedSystemId) resource.
///
/// To use the supplied input, the system should have a [`SystemInput`] as the first parameter.
pub fn run_system_cached_with<I, M, S>(system: S, input: I::Inner<'static>) -> impl Command
where
    I: SystemInput<Inner<'static>: Send> + Send + 'static,
    M: 'static,
    S: IntoSystem<I, (), M> + Send + 'static,
{
    move |world: &mut World| -> Result {
        world.run_system_cached_with(system, input)?;
        Ok(())
    }
}

/// A [`Command`] that removes a system previously registered with
/// [`Commands::register_system`](crate::system::Commands::register_system) or
/// [`World::register_system`].
pub fn unregister_system<I, O>(system_id: SystemId<I, O>) -> impl Command
where
    I: SystemInput + Send + 'static,
    O: Send + 'static,
{
    move |world: &mut World| -> Result {
        world.unregister_system(system_id)?;
        Ok(())
    }
}

/// A [`Command`] that removes a system previously registered with one of the following:
/// - [`Commands::run_system_cached`](crate::system::Commands::run_system_cached)
/// - [`World::run_system_cached`]
/// - [`World::register_system_cached`]
pub fn unregister_system_cached<I, O, M, S>(system: S) -> impl Command
where
    I: SystemInput + Send + 'static,
    O: 'static,
    M: 'static,
    S: IntoSystem<I, O, M> + Send + 'static,
{
    move |world: &mut World| -> Result {
        world.unregister_system_cached(system)?;
        Ok(())
    }
}

/// A [`Command`] that runs the schedule corresponding to the given [`ScheduleLabel`].
pub fn run_schedule(label: impl ScheduleLabel) -> impl Command {
    move |world: &mut World| -> Result {
        world.try_run_schedule(label)?;
        Ok(())
    }
}

/// Triggers the given [`Event`], which will run any [`Observer`]s watching for it.
///
/// [`Observer`]: crate::observer::Observer
#[track_caller]
pub fn trigger<'a, E: Event<Trigger<'a>: Default>>(event: E) -> impl Command {
    let caller = MaybeLocation::caller();
    PtrCommand::new(event, move |mut event, world| {
        world.trigger_ref_with_caller(&mut *event, &mut Default::default(), caller);
    })
}

/// Triggers the given [`Event`] using the given [`Trigger`], which will run any [`Observer`]s watching for it.
///
/// [`Trigger`]: crate::event::Trigger
/// [`Observer`]: crate::observer::Observer
#[track_caller]
pub fn trigger_with<E: Event<Trigger<'static>: Send + Sync>>(
    event: E,
    trigger: E::Trigger<'static>,
) -> impl Command {
    let caller = MaybeLocation::caller();
    PtrCommand::new((event, trigger), move |mut data, world| {
        let (event, trigger) = &mut *data;
        world.trigger_ref_with_caller(event, trigger, caller);
    })
}

/// A [`Command`] that writes an arbitrary [`Message`].
#[track_caller]
pub fn write_message<M: Message>(message: M) -> impl Command {
    let caller = MaybeLocation::caller();
    move |world: &mut World| {
        let mut messages = world.resource_mut::<Messages<M>>();
        messages.write_with_caller(message, caller);
    }
}

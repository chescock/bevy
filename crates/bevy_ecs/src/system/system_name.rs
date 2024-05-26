use crate::component::Tick;
use crate::prelude::World;
use crate::system::{ExclusiveSystemParam, ReadOnlySystemParam, SystemMeta, SystemParam};
use crate::world::unsafe_world_cell::UnsafeWorldCell;
use std::ops::Deref;

/// [`SystemParam`] that returns the name of the system which it is used in.
///
/// This is not a reliable identifier, it is more so useful for debugging or logging.
///
/// # Examples
///
/// ```
/// # use bevy_ecs::system::SystemName;
/// # use bevy_ecs::system::SystemParam;
///
/// #[derive(SystemParam)]
/// struct Logger<'s> {
///     system_name: SystemName<'s>,
/// }
///
/// impl<'s> Logger<'s> {
///     fn log(&mut self, message: &str) {
///         eprintln!("{}: {}", self.system_name, message);
///     }
/// }
///
/// fn system1(mut logger: Logger) {
///     // Prints: "crate_name::mod_name::system1: Hello".
///     logger.log("Hello");
/// }
/// ```
#[derive(Debug)]
pub struct SystemName<'s>(&'s str);

impl<'s> SystemName<'s> {
    /// Gets the name of the system.
    pub fn name(&self) -> &str {
        self.0
    }
}

impl<'s> Deref for SystemName<'s> {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        self.name()
    }
}

impl<'s> AsRef<str> for SystemName<'s> {
    fn as_ref(&self) -> &str {
        self.name()
    }
}

impl<'s> From<SystemName<'s>> for &'s str {
    fn from(name: SystemName<'s>) -> &'s str {
        name.0
    }
}

impl<'s> std::fmt::Display for SystemName<'s> {
    #[inline(always)]
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.name(), f)
    }
}

// SAFETY: no component value access
unsafe impl SystemParam for SystemName<'_> {
    type State = ();
    type Item<'w, 's> = SystemName<'s>;

    fn init_state(_world: &mut World, _system_meta: &mut SystemMeta) -> Self::State {}

    #[inline]
    unsafe fn get_param<'w, 's>(
        _state: &'s mut Self::State,
        system_meta: &'s SystemMeta,
        _world: UnsafeWorldCell<'w>,
        _last_run: Tick,
    ) -> Self::Item<'w, 's> {
        SystemName(&system_meta.name)
    }
}

// SAFETY: Only reads internal system state
unsafe impl<'s> ReadOnlySystemParam for SystemName<'s> {}

impl ExclusiveSystemParam for SystemName<'_> {
    type State = ();
    type Item<'s> = SystemName<'s>;

    fn init(_world: &mut World, _system_meta: &mut SystemMeta) -> Self::State {}

    fn get_param<'s>(_state: &'s mut Self::State, system_meta: &'s SystemMeta) -> Self::Item<'s> {
        SystemName(&system_meta.name)
    }
}

#[cfg(test)]
mod tests {
    use crate::system::SystemName;
    use crate::world::World;

    #[test]
    fn test_system_name_regular_param() {
        fn testing(name: SystemName) -> String {
            name.name().to_owned()
        }

        let mut world = World::default();
        let id = world.register_system(testing);
        let name = world.run_system(id).unwrap();
        assert!(name.ends_with("testing"));
    }

    #[test]
    fn test_system_name_exclusive_param() {
        fn testing(_world: &mut World, name: SystemName) -> String {
            name.name().to_owned()
        }

        let mut world = World::default();
        let id = world.register_system(testing);
        let name = world.run_system(id).unwrap();
        assert!(name.ends_with("testing"));
    }
}

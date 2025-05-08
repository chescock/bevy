---
title: Remove `ArchetypeComponentId`
pull_requests: [TODO]
---

Scheduling no longer uses `archetype_component_access` or `ArchetypeComponentId`.
To reduce memory usage and simplify the implementation, all uses of them have been removed.
Since we no longer need to update access before a system runs, `Query` now updates it state when the system runs instead of ahead of time.

The following methods have been deprecated:
* `System::update_archetype_component_access`
* `SystemParam::new_archetype`
* `SystemState::update_archetypes`
* `SystemState::update_archetypes_unsafe_world_cell`
* `SystemState::get_manual`
* `SystemState::get_manual_mut`

In addition, `SystemParam::validate_param` now takes `&mut Self::State` instead of `&Self::State` so that queries can update their caches.

If you were implementing the traits manually, move any logic from those methods into `System::validate_param_unsafe`, `System::run_unsafe`, `SystemParam::validate_param`, or `SystemParam::get_param`, and adjust the signature of `SystemParam::validate_param`.

If you were calling deprecated methods on the traits manually, the calls are no longer necessary and may simply be removed.
If you were calling `SystemState::get_manual(_mut)`, instead call `SystemState::get(_mut)`.

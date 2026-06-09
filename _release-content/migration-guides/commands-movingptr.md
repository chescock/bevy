---
title: Commands take `MovingPtr`
pull_requests: []
---

The `Command::apply` and `EntityCommand::apply` methods now take `self` by `MovingPtr` instead of by value.
This avoids the need to copy data from the command buffer onto the stack in order to run the command.

If you are implementing `Command` or `EntityCommand` manually, change the signature of `apply`.
To obtain a reference to the value, simply dereference the `MovingPtr`.
To obtain an owned value, call `read()`.
To obtain a `MovingPtr` to a field within the command, use `deconstruct_moving_ptr!`.

Consider replacing any manual implementations of `Command` or `EntityCommand` with a closure.
There is a blanket implementation for `FnOnce(&mut World)` and `FnOnce(EntityWorldMut)`.
If you want a `MovingPtr`, you can use `PtrCommand` or `PtrEntityCommand` to run a closure that accepts data from a `MovingPtr` to the command buffer.

```rust
// 0.19
impl Command for MyCommand {
    fn apply(self, world: &mut World) -> Self::Out {
        // ...
    }
}

// 0.20
impl Command for MyCommand {
    fn apply(this: MovingPtr<Self>, world: &mut World) -> Self::Out {
        // Use the `read()` method to copy the value to a local
        let this = this.read();
        // Alternatively, if you need a `MovingPtr` to
        // part of the command, use `deconstruct_moving_ptr!`
        deconstruct_moving_ptr!({
            let MyCommand { field_a, field_b } = this;
        });
    }
}
```

To invoke a command manually, use `move_as_ptr!` to create a `MovingPtr` from an owned value:

```rust
let command: impl Command = ...;
// 0.19
command.apply(&mut world);
// 0.20
move_as_ptr!(command);
Command::apply(command, &mut world);
```

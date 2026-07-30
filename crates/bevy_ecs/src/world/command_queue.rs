//! Defines the [`CommandQueue`] structure for densely storing [`Command`]s.  
//!
//! This acts like a `Vec<Box<dyn Command>>`, but stores the commands and metadata
//! in a single flat allocation that can be re-used across invocations.
//!
//! Since commands are used frequently in systems as a way to spawn
//! entities/components/resources, and it's not currently possible to parallelize these
//! due to mutable [`World`] access, maximizing performance for [`CommandQueue`] is
//! preferred to simplicity of implementation.
//!
//! # Command Records
//!
//! The command buffer stores a sequence of *command records*.
//! Each command record consists of a [`CommandMeta`]
//! followed by zero or more padding bytes and then the command data.
//! The start and end of the command record must be aligned for [`CommandMeta`],
//! and the end of the record must be aligned for the command data.
//!
//! If the command data has alignment greater than [`CommandMeta`],
//! then padding is required to align the command data.
//! The amount of padding will vary depending on the offset of the start of
//! the command record, so the size of the record will also vary.
//!
//! If the command data has alignment less than or equal to [`CommandMeta`],
//! then padding is only required to align the end of the command,
//! and the record has a constant size.
//! The padding is placed before the command data so that the data
//! is always at the end of the record.  This is different from a
//! `#[repr(C)] struct`, which would place padding *after* the data.
//!
//! # Alignment
//!
//! The size of a command record may depend on the offset of its start
//! modulo the command's alignment.
//! This means that the data can only be moved to a new location that has the
//! same offset modulo the largest alignment of any command in the buffer.
//!
//! To make that possible, each [`CommandQueue`] records the largest alignment of any
//! command it contains, and ensures that the buffer has at least that alignment.
//! When reallocating to a new buffer with the same or larger alignment,
//! command records can be copied to the same position in the new buffer.
//! When appending one buffer to another, padding commands may be needed
//! to ensure the buffer is aligned when copied.
//!
//! # Command Frames
//!
//! Each [`World`] has a special [owned command queue](World::command_queue).
//! Commands being run have access to `&mut World`, so this queue may be
//! pushed to or applied during the application of another command in the queue.
//!
//! The part of the `World`'s [`CommandQueue`] for each nested command is a *command frame*.
//!
//! Commands in earlier frames may have active references to data in the buffer,
//! so nested command frames must not deallocate that buffer or access that memory.
//!
//! A simple approach would be to [`take`](core::mem::take) the [`CommandQueue`] out of the `World`
//! while running commands and create a new queue for the nested command frame.
//! That would require managing a separate buffer for each command frame,
//! requiring overhead to manage the buffers,
//! and wasting space when a small command frame uses a large buffer.
//!
//! Instead, a single buffer is shared among multiple command frames.
//! While running a command, ownership of the buffer is transferred
//! out of the [`CommandQueue`] and into the [`WorldCommandQueueRunner`].
//! The tail of the buffer is then lent back to the queue for use in other frames.
//! When the original frame completes,
//! it can transfer ownership of the buffer back to the queue.
//!
//! If a nested frame running on a borrowed buffer needs to reallocate,
//! it allocates a larger buffer and copies only the data from the current frame.
//! The original buffer will remain alive until the frame that owns it completes,
//! at which point it will be deallocated instead of returned to the queue.
//!
//! For simplicity, the nested frames will start at the same offset in a
//! newly-allocated owned buffer that they started at in the borrowed buffer.
//! This preserves their alignment when copying, and ensures that the resulting
//! buffer is large enough to hold the full set of commands in the future.
//! However, this does mean the space in the beginning of the buffer goes unused
//! until the original command frame completes.
//!
//! Because nested frames borrow the buffer from earlier frames,
//! they must make sure they do not use it once the frame ends.
//! This cannot be checked with lifetimes, and instead relies on
//! [`World::command_queue`] not being exposed and
//! [`WorldCommandQueueRunner`] not being leaked.
use crate::{
    change_detection::MaybeLocation,
    system::{Command, SystemBuffer, SystemMeta},
    world::{DeferredWorld, World, WorldId},
};

use alloc::alloc::{alloc, dealloc, handle_alloc_error};
use bevy_ptr::MovingPtr;
use core::{
    alloc::Layout,
    fmt::Debug,
    hint::cold_path,
    marker::PhantomData,
    mem::{forget, size_of, ManuallyDrop},
    num::NonZero,
    ptr::NonNull,
};
use log::warn;

#[cfg(feature = "std")]
use crate::error::{BevyError, ErrorContext, Severity, PANIC_ORIGINATES_FROM_ERROR_HANDLER};
#[cfg(feature = "std")]
use alloc::boxed::Box;
#[cfg(feature = "std")]
use bevy_utils::DebugName;
#[cfg(feature = "std")]
use std::{
    backtrace::Backtrace,
    panic::{catch_unwind, resume_unwind, AssertUnwindSafe},
};

/// Metadata describing the runtime type of a [command record].
///
/// [command record]: self#command-records
struct CommandMeta {
    /// Consumes a command and advances `cursor` to the beginning of the next [command record].
    ///
    /// If `world` is `Some`, applies the command to that `world`.
    ///
    /// If `world` is `None`, drops the command without applying it.
    ///
    /// # Safety
    ///
    /// The cursor must point to the beginning of a command record
    /// that was written with this [`CommandMeta`].
    ///
    /// [command record]: self#command-records
    consume_and_advance: unsafe fn(cursor: &mut NonNull<u8>, world: Option<&mut World>),
}

/// A type used to calculate the maximum size and alignment of a [command record].
/// This is `#[repr(C)]` so that the layout is deterministic.
///
/// Note that this does *not* always match the actual layout of a [command record]!
/// Commands with alignment less than `CommandMeta` will be written
/// with padding before the command instead of after.
/// Commands with alignment larger than `CommandMeta` may be written with less
/// padding depending on the address of the `CommandMeta`.
///
/// [command record]: self#command-records
#[repr(C)]
struct MetaAndCommand<C> {
    _meta: CommandMeta,
    _command: C,
}

/// Densely and efficiently stores a queue of heterogenous types implementing [`Command`].
#[derive(Debug)]
pub struct CommandQueue {
    /// This buffer densely stores all queued commands
    /// as a sequence of [command record]s.
    ///
    /// To interpret these bytes, a pointer must
    /// be passed to the corresponding [`CommandMeta::consume_and_advance`] fn pointer.
    ///
    /// [command record]: self#command-records
    buffer: NonNull<u8>,
    /// The `Layout` used to allocate the buffer.
    /// The alignment will be greater than or equal to
    /// the alignment of all commands in the buffer.
    layout: Layout,
    /// The start of the current [command frame].
    ///
    /// This is only nonzero when running a nested [command frame].
    ///
    /// Bytes earlier than this in the buffer may have active references
    /// to them and should not be accessed in any way.
    ///
    /// [command frame]: self#command-frames
    start: usize,
    /// The end of the current [command frame].
    ///
    /// All bytes after this are uninitialized,
    /// and the next [command record] will be written here.
    ///
    /// [command record]: self#command-records
    /// [command frame]: self#command-frames
    end: usize,
    /// Whether this command queue owns [`Self::buffer`]
    /// and should deallocate it when it is no longer needed.
    ///
    /// This is `false` when [`Self::layout`] has zero size,
    /// or when using a borrowed buffer in a nested [command frame].
    ///
    /// [command frame]: self#command-frames
    owned: bool,
    /// The source location that created this command queue.
    caller: MaybeLocation,
    /// Always emit a warning if a command is dropped before it is applied.
    /// Defaults to `true`.
    ///
    /// This setting can be turned off for commands that might be dropped (due to application exit) before those
    /// commands are applied in ordinary situations, for example delayed commands.
    warn_on_unapplied: bool,
}

/// Minimum alignment for the `CommandQueue`'s internal buffer.
///
/// Reserving space for a command with alignment less than or equal to
/// this can be done without a runtime alignment check.
///
/// Most memory allocators return buffers with 16-byte alignment,
/// so setting this to 16 is usually free, and ensures that
/// `Vec3A` can be included in a command without a runtime alignment check.
const MIN_COMMAND_QUEUE_ALIGN: NonZero<usize> = NonZero::new(16).unwrap();

impl Default for CommandQueue {
    #[track_caller]
    fn default() -> Self {
        Self {
            buffer: NonNull::without_provenance(MIN_COMMAND_QUEUE_ALIGN),
            layout: Layout::from_size_align(0, MIN_COMMAND_QUEUE_ALIGN.get()).unwrap(),
            start: 0,
            end: 0,
            owned: false,
            caller: MaybeLocation::caller(),
            warn_on_unapplied: true,
        }
    }
}

// SAFETY: All commands [`Command`] implement [`Send`]
unsafe impl Send for CommandQueue {}

// SAFETY: `&CommandQueue` never gives access to the inner commands.
unsafe impl Sync for CommandQueue {}

impl CommandQueue {
    /// Create a queue that does not warn when dropped.
    #[track_caller]
    pub fn silent() -> Self {
        CommandQueue {
            buffer: NonNull::without_provenance(MIN_COMMAND_QUEUE_ALIGN),
            layout: Layout::from_size_align(0, MIN_COMMAND_QUEUE_ALIGN.get()).unwrap(),
            start: 0,
            end: 0,
            owned: false,
            caller: MaybeLocation::caller(),
            warn_on_unapplied: false,
        }
    }

    /// Push a [`Command`] onto the queue.
    #[inline]
    pub fn push<C: Command<Out = ()>>(&mut self, command: C) {
        let meta = CommandMeta {
            consume_and_advance: |cursor, mut world| {
                // SAFETY: This advances to the end of the command record, which is within the buffer
                *cursor = unsafe { advance_to_command_end::<C>(*cursor) };

                // SAFETY: This points to the command, which is within the buffer
                let command = unsafe { cursor.sub(size_of::<C>()) };

                // SAFETY: `cursor` pointed to the beginning of this command buffer when called,
                // so it points to the end now, so `command` points to the command data.
                // It is safe to transfer ownership, since the increment of `cursor` above
                // guarantees that nothing stored in the buffer will get observed after this function ends.
                let command = unsafe { MovingPtr::<C>::new(command.cast()) };

                let f = || {
                    match world.as_deref_mut() {
                        // Apply command to the provided world...
                        Some(world) => {
                            C::apply(command, world);
                            // The command may have queued up world commands, which we flush here to ensure they are also picked up.
                            // If the current command queue already the World Command queue, this will still behave appropriately because the global cursor
                            // is still at the current `stop`, ensuring only the newly queued Commands will be applied.
                            world.flush();
                        }
                        // ...or discard it.
                        None => drop(command),
                    }
                };

                #[cfg(feature = "std")]
                {
                    let result = catch_unwind(AssertUnwindSafe(f));
                    if let Err(payload) = result {
                        let name = DebugName::type_name::<C>();
                        handle_panic_payload(world, payload, name);
                    }
                }

                #[cfg(not(feature = "std"))]
                (f)();
            },
        };

        // Reserve enough bytes for both the metadata and the command itself.
        self.reserve(Layout::new::<MetaAndCommand<C>>());

        // Write the command record by writing the meta,
        // then advancing the pointer and writing the data.
        let end_ptr = self.end_ptr();
        // SAFETY: `reserve` ensures this is within the buffer
        unsafe { end_ptr.cast().write(meta) };
        // SAFETY: `reserve` ensures this is within the buffer
        let command_end = unsafe { advance_to_command_end::<C>(end_ptr) };
        // SAFETY: `reserve` ensures this is within the buffer,
        // and `advance_to_command_end` ensures that it is aligned.
        unsafe { command_end.sub(size_of::<C>()).cast().write(command) };

        // Update the end pointer to the end of the new record.
        // SAFETY: `command_end` is within the buffer, and `buffer` is the start.
        self.end = unsafe { command_end.offset_from_unsigned(self.buffer) };
    }

    /// Ensures there is enough capacity in the buffer to
    /// append an aligned value at the given layout.
    #[inline]
    fn reserve(&mut self, layout: Layout) {
        // Skip the runtime alignment check if the alignment is already
        // less than or equal to the default command queue alignment.
        if layout.align() > MIN_COMMAND_QUEUE_ALIGN.get() && layout.align() > self.layout.align()
            || self.layout.size() - self.end < layout.size()
        {
            self.do_reserve(layout);
        }
    }

    /// Allocates a new buffer with enough capacity in the buffer to
    /// append an aligned value at the given layout.
    ///
    /// This is separate from [`Self::reserve`] so that the checks
    /// can be `#[inline]`d into the caller.
    #[cold]
    fn do_reserve(&mut self, layout: Layout) {
        if self.layout.size() == 0 && layout.size() == 0 {
            // Don't allocate for a ZST, but make sure the pointer is sufficiently aligned.
            if layout.align() > self.layout.align() {
                self.layout = layout;
                self.buffer = NonNull::without_provenance(NonZero::new(layout.align()).unwrap());
            }
            return;
        }

        // Note that we do not use `alloc::realloc`.
        // One reason is that it does not let us change alignment,
        // and this method sometimes needs to increase the alignment of the buffer.
        // The other reason is that when running a nested [command frame],
        // there may be active references earlier in the buffer
        // to commands in other frames.

        // `end + layout.size()` and `size() * 2` cannot wrap.
        // `Layout` ensures the size is less than or equal to `isize::MAX`,
        // so doubling it cannot overflow `usize`, and `end < layout.size()`.
        let desired_size = self.end + layout.size();
        let new_size = if desired_size <= self.layout.size() {
            // If we have enough capacity but need to increase alignment,
            // keep the existing capacity.
            self.layout.size()
        } else if desired_size <= self.layout.size() * 2 {
            // Ensure the capacity doubles to amortize growth.
            self.layout.size() * 2
        } else {
            // Ensure we have enough capacity for the desired size.
            desired_size
        };
        let new_align = self.layout.align().max(layout.align());
        let new_layout = Layout::from_size_align(new_size, new_align)
            .unwrap()
            .pad_to_align();
        let old_buffer = self.buffer;
        let old_layout = self.layout;
        // SAFETY: If `new_size` would have been zero, the method would have returned early.
        let new_buffer = NonNull::new(unsafe { alloc(new_layout) })
            .unwrap_or_else(|| handle_alloc_error(new_layout));

        // Only copy the current [command frame],
        // and only deallocate if the frame is owned.
        // When running a nested [command frame],
        // there may be active references to earlier bytes,
        // so the old allocation must be left until the frame completes.
        // SAFETY:
        // - The memory from `buffer + start` to `buffer + end` is valid for reads
        //   and not accessed by earlier command frames.
        // - The memory from `new_buffer + start` to `new_buffer + end` is valid for
        //   writes and not accessed anywhere else.
        // - The buffers were returned from different calls to `alloc`
        //   and cannot alias.
        unsafe {
            self.start_ptr()
                .copy_to_nonoverlapping(new_buffer.add(self.start), self.end - self.start);
        }
        if self.owned {
            // SAFETY: This pointer was returned from `alloc(old_layout)`
            // and not deallocated elsewhere.
            unsafe { dealloc(old_buffer.as_ptr(), old_layout) };
        }
        self.buffer = new_buffer;
        self.layout = new_layout;
        self.owned = true;
    }

    /// A pointer to the start of the current [command frame].
    ///
    /// [command frame]: self#command-frames
    #[inline]
    fn start_ptr(&self) -> NonNull<u8> {
        // SAFETY: `start` is within the bounds of `buffer`'s allocation
        unsafe { self.buffer.add(self.start) }
    }

    /// A pointer to the end of the current [command frame],
    /// where the next command will be written.
    ///
    /// [command frame]: self#command-frames
    #[inline]
    fn end_ptr(&self) -> NonNull<u8> {
        // SAFETY: `end` is within the bounds of `buffer`'s allocation
        unsafe { self.buffer.add(self.end) }
    }

    /// Execute the queued [`Command`]s in the world after applying any commands in the world's internal queue.
    /// This clears the queue.
    #[inline]
    pub fn apply(&mut self, world: &mut World) {
        // flush the world's internal queue
        world.flush_commands();

        let mut runner = CommandQueueRunner::new(self);
        runner.run(Some(world));
        // If the commands all executed, then there is nothing to drop,
        // but the compiler does not seem to optimize out the loop.
        forget(runner);
    }

    /// Execute the queued [`Command`]s in the queue owned by the given [`World`].
    pub(crate) fn flush_world_commands(world: &mut World) {
        WorldCommandQueueRunner::new(world).apply_queued();
    }

    /// Take all commands from `other` and append them to `self`, leaving `other` empty
    pub fn append(&mut self, other: &mut CommandQueue) {
        // Don't even reserve space for the alignment command if there is nothing to copy.
        if other.end == 0 {
            return;
        }

        // This would be possible to support, but is never needed.
        assert_eq!(
            other.start, 0,
            "`append` cannot be used to remove commands from a `World`'s command queue"
        );

        // Reserve enough space for the other command queue
        // plus an optional [command record] to align the other queue.
        let command_layout = Layout::new::<MetaAndCommand<usize>>();
        let max_padding = other.layout.align().saturating_sub(command_layout.align());
        let padding_layout = Layout::array::<u8>(max_padding).unwrap();
        let padded_command_layout = command_layout.extend_packed(padding_layout).unwrap();
        self.reserve(padded_command_layout.extend(other.layout).unwrap().0);

        // Ensure `other` is aligned within `self`.
        // If padding is required, add a [command record] that will
        // advance the cursor to the next aligned address.
        // Store the padding required as the data for that command.
        let end_ptr = self.end_ptr();
        if end_ptr.align_offset(other.layout.align()) != 0 {
            let meta = CommandMeta {
                consume_and_advance: |cursor, _world| {
                    // The offset is stored as the command data.

                    // SAFETY: This advances to the end of the command record, which is within the buffer
                    let command_end = unsafe { advance_to_command_end::<usize>(*cursor) };
                    // SAFETY: This points to the data, which is within the buffer
                    let offset: usize =
                        unsafe { command_end.sub(size_of::<usize>()).cast().read() };

                    // SAFETY: `offset` stays within the buffer
                    *cursor = unsafe { cursor.add(offset) };
                },
            };
            // SAFETY: `reserve` ensures this is within the buffer
            let command_end = unsafe { advance_to_command_end::<usize>(end_ptr) };
            let padding = command_end.align_offset(other.layout.align());
            // SAFETY: `reserve` ensures this is within the buffer,
            // and `command_end` is later than `end_ptr`.
            let offset = unsafe { command_end.add(padding).offset_from_unsigned(end_ptr) };
            // SAFETY: `reserve` ensures this is within the buffer
            unsafe { end_ptr.cast().write(meta) };
            // SAFETY: `reserve` ensures this is within the buffer
            // `advance_to_command_end` ensures this is aligned
            unsafe { command_end.sub(size_of::<usize>()).cast().write(offset) };
            self.end += offset;
        }

        let end_ptr = self.end_ptr();
        // SAFETY:
        // - The memory from `other.buffer` to `other.buffer + other.end` is valid for reads
        //   and not accessed by earlier command frames.
        // - `reserve` ensures the memory from `self.buffer + end` to `self.buffer + end + other.end`
        //   is valid for writes and not accessed anywhere else.
        // - The buffers were returned from different calls to `alloc`
        //   and cannot alias.
        unsafe { other.buffer.copy_to_nonoverlapping(end_ptr, other.end) };

        self.end += other.end;
        other.end = 0;
    }

    /// Returns false if there are any commands in the queue
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.end == self.start
    }

    /// Silences drop warning if commands are unapplied.
    pub fn silence_drop_warning(&mut self) {
        self.warn_on_unapplied = false;
    }
}

/// Advances the cursor from the start of a [command record] to its end.
///
/// # Safety
///
/// The allocation must have enough capacity to fit the full [command record].
///
/// [command record]: self#command-records
unsafe fn advance_to_command_end<C>(cursor: NonNull<u8>) -> NonNull<u8> {
    // Add space for `CommandMeta` and `C`, plus padding in between.
    if align_of::<C>() <= align_of::<CommandMeta>() {
        // The start pointer is already aligned for `CommandMeta`,
        // so it is also aligned for `C` and this can be a constant offset.
        // SAFETY: Caller ensures this is within the allocation
        unsafe { cursor.add(size_of::<MetaAndCommand<C>>()) }
    } else {
        // SAFETY: Caller ensures this is within the allocation
        let unaligned = unsafe { cursor.add(size_of::<CommandMeta>() + size_of::<C>()) };
        // Add padding to ensure `C` is aligned.
        // This will also align the end of the [command record] for `CommandMeta`,
        // since it has smaller alignment.
        let padding = unaligned.align_offset(align_of::<C>());
        // SAFETY: Caller ensures this is within the allocation
        unsafe { unaligned.add(padding) }
    }
}

impl Drop for CommandQueue {
    fn drop(&mut self) {
        if !self.is_empty() && self.warn_on_unapplied {
            if let Some(caller) = self.caller.into_option() {
                warn!("CommandQueue has un-applied commands being dropped. Did you forget to call SystemState::apply? caller:{caller:?}");
            } else {
                warn!("CommandQueue has un-applied commands being dropped. Did you forget to call SystemState::apply?");
            }
        }
        // Drop any commands in the queue by creating a `CommandQueueRunner` and dropping it.
        drop(CommandQueueRunner::new(self));
        if self.owned {
            // SAFETY: This pointer was returned from `alloc(self.layout)`
            // and not deallocated elsewhere.
            unsafe { dealloc(self.buffer.as_ptr(), self.layout) };
        }
    }
}

impl SystemBuffer for CommandQueue {
    #[inline]
    fn apply(&mut self, _system_meta: &SystemMeta, world: &mut World) {
        #[cfg(feature = "trace")]
        let _span_guard = _system_meta.commands_span.enter();
        self.apply(world);
    }

    #[inline]
    fn queue(&mut self, _system_meta: &SystemMeta, mut world: DeferredWorld) {
        world.commands().append(self);
    }
}

/// A RAII guard used while running commands to ensure
/// that unapplied commands are dropped during unwind.
struct CommandQueueRunner<'a> {
    /// A pointer to the next [command record],
    /// or [`Self::end`] if there are no more commands.
    ///
    /// [command record]: self#command-records
    cursor: NonNull<u8>,
    /// The end of the current [command frame].
    ///
    /// [command frame]: self#command-frames
    end: NonNull<u8>,
    /// Use a lifetime to ensure that nothing else accesses the queue,
    /// but don't actually store a reference anywhere.
    marker: PhantomData<&'a mut CommandQueue>,
}

impl<'a> CommandQueueRunner<'a> {
    fn new(command_queue: &'a mut CommandQueue) -> Self {
        let cursor = command_queue.start_ptr();
        let end = command_queue.end_ptr();
        // Empty the queue by setting the end of the [command frame] to the start.
        // Doing this now instead of during `drop` means the reference does not need
        // to be stored, and nothing else can access the queue to observe the difference.
        command_queue.end = command_queue.start;
        Self {
            cursor,
            end,
            marker: PhantomData,
        }
    }

    fn run(&mut self, mut world: Option<&mut World>) {
        #[cfg(feature = "std")]
        {
            PANIC_ORIGINATES_FROM_ERROR_HANDLER.set(false);
        }

        while self.cursor < self.end {
            // Read the metadata for the next [command record].

            // SAFETY: The cursor always points to the start of a [command record]
            // or to the end of the [command frame].
            // The loop checked that it was in bounds, so it points to a valid [command record].
            let meta = unsafe { self.cursor.cast::<CommandMeta>().read() };

            // Consume the command and advance the cursor.
            // At this point, it will either point to the next [command record],
            // or the cursor will be out of bounds and the loop will end.

            // SAFETY: `cursor` points to a command record, and this was its `CommandMeta`.
            unsafe { (meta.consume_and_advance)(&mut self.cursor, world.as_deref_mut()) };
        }
    }
}

/// Handle a panic thrown within a command.
///
/// This is a separate non-generic function so that the panic handling code
/// is not monomorphized separately for each command type.
#[cfg(feature = "std")]
#[cold]
fn handle_panic_payload(
    world: Option<&mut World>,
    payload: Box<dyn core::any::Any + Send>,
    name: DebugName,
) {
    let panic_originates_from_error_handler = PANIC_ORIGINATES_FROM_ERROR_HANDLER.replace(false);
    if panic_originates_from_error_handler {
        resume_unwind(payload)
    }
    let Some(world) = world else {
        resume_unwind(payload)
    };
    let error =
        BevyError::new_with_backtrace(Severity::Panic, "Command panicked", Backtrace::disabled());
    world.fallback_error_handler()(error, ErrorContext::Command { name });
}

impl Drop for CommandQueueRunner<'_> {
    fn drop(&mut self) {
        // Drop any unapplied commands.
        // If `run` completed successfully then this will do nothing.
        self.run(None);
    }
}

/// Holds the [`World`]'s main command queue while running a [command frame],
/// and ensures ownership is returned to the [`World`] even if the commands panic.
///
/// [command frame]: self#command-frames
struct WorldCommandQueueRunner<'w> {
    world_id: WorldId,
    world: &'w mut World,
    command_queue: ManuallyDrop<CommandQueue>,
}

impl<'a> WorldCommandQueueRunner<'a> {
    fn new(world: &'a mut World) -> Self {
        // Transfer ownership of the `World`'s main command queue into the runner,
        // but lend the tail of the buffer back to the
        // `World` for use in nested [command frame]s.

        // The nested [command frame] must not continue to use the buffer
        // once the frame ends, but this cannot be checked with lifetimes.
        // Instead this relies on `World::command_queue` not being exposed
        // and `WorldCommandQueueRunner` not being leaked.

        let world_queue = world.command_queue.get_mut();
        let command_queue = ManuallyDrop::new(CommandQueue { ..*world_queue });
        // `owned` may have already been `false`, but it's harmless to set it again.
        world_queue.owned = false;
        world_queue.start = command_queue.end;
        Self {
            world_id: world.id(),
            world,
            command_queue,
        }
    }

    fn apply_queued(&mut self) {
        let mut runner = CommandQueueRunner::new(&mut self.command_queue);
        runner.run(Some(self.world));
        // If the commands all executed, then there is nothing to drop,
        // but the compiler does not seem to optimize out the loop.
        forget(runner);
    }
}

impl Drop for WorldCommandQueueRunner<'_> {
    fn drop(&mut self) {
        // End the lease on the tail of the buffer and then
        // return ownership of the [command frame] to the `World`.

        // `world_queue` must be the same `CommandQueue` the tail was lent to
        // for it to be sound to end the lease.
        // Ensuring this relies on `World::command_queue` not being exposed
        // and `WorldCommandQueueRunner` not being leaked.

        let world_queue = self.world.command_queue.get_mut();
        if self.world_id != self.world.id {
            handle_swapped_world(world_queue, self.command_queue.end);
            #[cold]
            fn handle_swapped_world(world_queue: &mut CommandQueue, end: usize) {
                // It's possible for a command to replace the `World` itself.
                // In that case, there may be references to the buffer
                // through the original world, so the lease cannot be ended
                // and any queue owned by this runner must be leaked.
                // Ensure the world's queue is owned, since that is the only
                // way to be sure that no other commands are using part of it.
                // Ensure `end <= layout.size()` so that `end_ptr()` is valid.
                warn!("World was replaced during command application.");
                if !world_queue.owned || world_queue.layout.size() < end {
                    // Ensure the size is nonzero and that `world_queue.end <= world_queue.layout.size()`.
                    let layout = Layout::from_size_align(end.max(1), world_queue.layout.align());
                    let layout = layout.unwrap().pad_to_align();
                    // SAFETY: `max(1)` ensures the size is nonzero
                    let buffer = NonNull::new(unsafe { alloc(layout) })
                        .unwrap_or_else(|| handle_alloc_error(layout));
                    if world_queue.owned {
                        // SAFETY: This pointer was returned from `alloc(world_queue.layout)`
                        // and not deallocated elsewhere.
                        unsafe { dealloc(world_queue.buffer.as_ptr(), world_queue.layout) };
                    }
                    world_queue.layout = layout;
                    world_queue.buffer = buffer;
                    world_queue.owned = true;
                }
            }
        } else if self.command_queue.owned {
            if world_queue.owned {
                cold_path();
                // A larger buffer has been allocated,
                // so free the older buffer.
                // SAFETY: This pointer was returned from `alloc(self.command_queue.layout)`
                // and not deallocated elsewhere.
                unsafe {
                    dealloc(
                        self.command_queue.buffer.as_ptr(),
                        self.command_queue.layout,
                    );
                }
            } else {
                // Return ownership of the buffer back to the world's queue.
                world_queue.owned = true;
            }
        }
        // Note that `self.command_queue.start == self.command_queue.end`,
        // because the `CommandQueueRunner` emptied the queue.
        world_queue.start = self.command_queue.start;
        world_queue.end = self.command_queue.start;
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        component::Component, error::FallbackErrorHandler, resource::Resource,
        system::command::PtrCommand,
    };
    use alloc::{
        borrow::ToOwned,
        string::{String, ToString},
        sync::Arc,
        vec::Vec,
    };
    use bevy_ptr::MovingPtr;
    use core::{
        panic::AssertUnwindSafe,
        sync::atomic::{AtomicU32, Ordering},
    };
    use std::sync::Mutex;

    struct DropCheck(Arc<AtomicU32>);

    impl DropCheck {
        fn new() -> (Self, Arc<AtomicU32>) {
            let drops = Arc::new(AtomicU32::new(0));
            (Self(drops.clone()), drops)
        }
    }

    impl Drop for DropCheck {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl Command for DropCheck {
        type Out = ();

        fn apply(_this: MovingPtr<Self>, _: &mut World) {}
    }

    #[test]
    fn test_command_queue_inner_drop() {
        let mut queue = CommandQueue::default();

        let (dropcheck_a, drops_a) = DropCheck::new();
        let (dropcheck_b, drops_b) = DropCheck::new();

        queue.push(dropcheck_a);
        queue.push(dropcheck_b);

        assert_eq!(drops_a.load(Ordering::Relaxed), 0);
        assert_eq!(drops_b.load(Ordering::Relaxed), 0);

        let mut world = World::new();
        queue.apply(&mut world);

        assert_eq!(drops_a.load(Ordering::Relaxed), 1);
        assert_eq!(drops_b.load(Ordering::Relaxed), 1);
    }

    /// Asserts that inner [commands](`Command`) are dropped on early drop of [`CommandQueue`].
    /// Originally identified as an issue in [#10676](https://github.com/bevyengine/bevy/issues/10676)
    #[test]
    fn test_command_queue_inner_drop_early() {
        let mut queue = CommandQueue::default();

        let (dropcheck_a, drops_a) = DropCheck::new();
        let (dropcheck_b, drops_b) = DropCheck::new();

        queue.push(dropcheck_a);
        queue.push(dropcheck_b);

        assert_eq!(drops_a.load(Ordering::Relaxed), 0);
        assert_eq!(drops_b.load(Ordering::Relaxed), 0);

        drop(queue);

        assert_eq!(drops_a.load(Ordering::Relaxed), 1);
        assert_eq!(drops_b.load(Ordering::Relaxed), 1);
    }

    #[derive(Component)]
    struct A;

    struct SpawnCommand;

    impl Command for SpawnCommand {
        type Out = ();

        fn apply(_this: MovingPtr<Self>, world: &mut World) {
            world.spawn(A);
        }
    }

    #[test]
    fn test_command_queue_inner() {
        let mut queue = CommandQueue::default();

        queue.push(SpawnCommand);
        queue.push(SpawnCommand);

        let mut world = World::new();
        queue.apply(&mut world);

        assert_eq!(world.query::<&A>().query(&world).count(), 2);

        // The previous call to `apply` cleared the queue.
        // This call should do nothing.
        queue.apply(&mut world);
        assert_eq!(world.query::<&A>().query(&world).count(), 2);
    }

    #[expect(
        dead_code,
        reason = "The inner string is used to ensure that, when the PanicCommand gets pushed to the queue, some data is written to the `bytes` vector."
    )]
    struct PanicCommand(String);
    impl Command for PanicCommand {
        type Out = ();

        fn apply(_this: MovingPtr<Self>, _: &mut World) {
            panic!("command is panicking");
        }
    }

    #[test]
    fn test_command_queue_inner_panic_safe_panic() {
        let mut queue = CommandQueue::default();

        queue.push(PanicCommand("I panic!".to_owned()));
        // This will get skipped due to the panic
        queue.push(SpawnCommand);

        let mut world = World::new();

        let _ = catch_unwind(AssertUnwindSafe(|| {
            queue.apply(&mut world);
        }));

        // Even though the first command panicked, it's still ok to push
        // more commands.
        queue.push(SpawnCommand);
        queue.push(SpawnCommand);
        queue.apply(&mut world);
        assert_eq!(world.query::<&A>().query(&world).count(), 2);
    }

    #[test]
    fn test_command_queue_inner_panic_safe_handled() {
        let mut queue = CommandQueue::default();

        queue.push(PanicCommand("I panic!".to_owned()));
        // This will get run because the fallback error handler
        // handles the panicking command.
        queue.push(SpawnCommand);

        fn record_last_error(error: BevyError, context: ErrorContext) {
            *LAST_ERROR.lock().unwrap() = Some((error, context));
        }
        static LAST_ERROR: Mutex<Option<(BevyError, ErrorContext)>> = Mutex::new(None);
        *LAST_ERROR.lock().unwrap() = None;

        let mut world = World::new();
        world.insert_resource(FallbackErrorHandler(record_last_error));

        queue.apply(&mut world);

        // Even though the first command panicked, it's still ok to push
        // more commands.
        queue.push(SpawnCommand);
        queue.push(SpawnCommand);
        queue.apply(&mut world);
        assert_eq!(world.query::<&A>().query(&world).count(), 3);

        let (error, context) = LAST_ERROR.lock().unwrap().take().unwrap();
        assert!(error.to_string().contains("Command panicked"));
        let name = DebugName::type_name::<PanicCommand>();
        assert_eq!(context, ErrorContext::Command { name });
    }

    #[test]
    fn test_command_queue_inner_nested_panic_safe_panic() {
        #[derive(Resource, Default)]
        struct Order(Vec<usize>);

        let mut world = World::new();
        world.init_resource::<Order>();

        fn add_index(index: usize) -> impl Command {
            move |world: &mut World| world.resource_mut::<Order>().0.push(index)
        }
        world.commands().queue(add_index(1));
        world.commands().queue(|world: &mut World| {
            world.commands().queue(add_index(2));
            world.commands().queue(PanicCommand("I panic!".to_owned()));
            // Everything after here will get skipped due to the panic
            world.commands().queue(add_index(3));
            world.flush_commands();
        });
        world.commands().queue(add_index(4));

        let _ = catch_unwind(AssertUnwindSafe(|| {
            world.flush_commands();
        }));

        world.commands().queue(add_index(5));
        world.flush_commands();
        assert_eq!(&world.resource::<Order>().0, &[1, 2, 5]);
    }

    #[test]
    fn test_command_queue_inner_nested_panic_safe_handled() {
        #[derive(Resource, Default)]
        struct Order(Vec<usize>);

        fn record_last_error(error: BevyError, context: ErrorContext) {
            *LAST_ERROR.lock().unwrap() = Some((error, context));
        }
        static LAST_ERROR: Mutex<Option<(BevyError, ErrorContext)>> = Mutex::new(None);
        *LAST_ERROR.lock().unwrap() = None;

        let mut world = World::new();
        world.init_resource::<Order>();
        world.insert_resource(FallbackErrorHandler(record_last_error));

        fn add_index(index: usize) -> impl Command {
            move |world: &mut World| world.resource_mut::<Order>().0.push(index)
        }
        world.commands().queue(add_index(1));
        world.commands().queue(|world: &mut World| {
            world.commands().queue(add_index(2));
            world.commands().queue(PanicCommand("I panic!".to_owned()));
            // Everything after here will get run because the
            // fallback error handler handles the panicking command.
            world.commands().queue(add_index(3));
            world.flush_commands();
        });
        world.commands().queue(add_index(4));

        world.flush_commands();

        world.commands().queue(add_index(5));
        world.flush_commands();
        assert_eq!(&world.resource::<Order>().0, &[1, 2, 3, 4, 5]);

        let (error, context) = LAST_ERROR.lock().unwrap().take().unwrap();
        assert!(error.to_string().contains("Command panicked"));
        let name = DebugName::type_name_of_val(&PanicCommand(String::new()).handle_error());
        assert_eq!(context, ErrorContext::Command { name });
    }

    // NOTE: `CommandQueue` is `Send` because `Command` is send.
    // If the `Command` trait gets reworked to be non-send, `CommandQueue`
    // should be reworked.
    // This test asserts that Command types are send.
    fn assert_is_send_impl(_: impl Send) {}
    fn assert_is_send(command: impl Command) {
        assert_is_send_impl(command);
    }

    #[test]
    fn test_command_is_send() {
        assert_is_send(SpawnCommand);
    }

    #[expect(
        dead_code,
        reason = "This struct is used to test how the CommandQueue reacts to padding added by rust's compiler."
    )]
    struct CommandWithPadding(u8, u16);
    impl Command for CommandWithPadding {
        type Out = ();

        fn apply(_this: MovingPtr<Self>, _: &mut World) {}
    }

    /// Creates a command for testing buffer sizes that includes a value
    /// of the given type and ensures it has unique access while running.
    fn test_command<T: Default + Send + 'static>(
        func: impl FnOnce(&mut World) + Send + 'static,
    ) -> impl Command<Out = ()> {
        PtrCommand::new(T::default(), |mut data, world| {
            // Help miri detect aliasing access to the command buffer.
            // Create a mutable reference to the buffer,
            // and use it to write both before and after running the nested command.
            let data = &mut *data;
            *data = T::default();
            func(world);
            *data = T::default();
        })
    }

    /// Creates a command for testing buffer sizes that includes a value
    /// of the given type and ensures it has unique access while running.
    fn empty_test_command<T: Default + Send + 'static>() -> impl Command<Out = ()> {
        test_command::<T>(|_world| {})
    }

    #[cfg(target_pointer_width = "64")]
    #[repr(align(32))]
    #[derive(Default)]
    struct Align32 {
        _padding: u8,
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn command_queue_size() {
        let mut world = World::new();

        // An empty command still uses space for the `CommandMeta`.
        let mut command_queue = CommandQueue::default();
        command_queue.push(empty_test_command::<()>());
        assert_eq!(MIN_COMMAND_QUEUE_ALIGN.get(), command_queue.layout.align());
        assert_eq!(8, command_queue.end);
        command_queue.apply(&mut world);

        // A command with equal alignment never needs extra padding.
        let mut command_queue = CommandQueue::default();
        command_queue.push(empty_test_command::<[u64; 2]>());
        assert_eq!(MIN_COMMAND_QUEUE_ALIGN.get(), command_queue.layout.align());
        assert_eq!(24, command_queue.end);
        command_queue.apply(&mut world);

        // A command with smaller alignment adds padding
        // to align the next `CommandMeta`.
        let mut command_queue = CommandQueue::default();
        command_queue.push(empty_test_command::<[u8; 9]>());
        assert_eq!(MIN_COMMAND_QUEUE_ALIGN.get(), command_queue.layout.align());
        assert_eq!(24, command_queue.end);
        command_queue.apply(&mut world);

        // That may be zero extra alignment.
        let mut command_queue = CommandQueue::default();
        command_queue.push(empty_test_command::<[u8; 16]>());
        assert_eq!(MIN_COMMAND_QUEUE_ALIGN.get(), command_queue.layout.align());
        assert_eq!(24, command_queue.end);
        command_queue.apply(&mut world);

        // A command with larger alignment adds padding to align the command.
        let mut command_queue = CommandQueue::default();
        command_queue.push(empty_test_command::<Align32>());
        assert_eq!(32, command_queue.layout.align());
        assert_eq!(64, command_queue.end);
        command_queue.apply(&mut world);

        // It may need less or zero padding if the command queue
        // was sufficiently aligned after the `CommandMeta` was written.
        let mut command_queue = CommandQueue::default();
        command_queue.push(empty_test_command::<[u64; 2]>());
        command_queue.push(empty_test_command::<Align32>());
        assert_eq!(32, command_queue.layout.align());
        assert_eq!(64, command_queue.end);
        command_queue.apply(&mut world);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn command_queue_append_size() {
        let mut world = World::new();

        // An empty target queue never needs an alignment command.
        let mut source_queue = CommandQueue::default();
        let mut target_queue = CommandQueue::default();
        source_queue.push(empty_test_command::<Align32>());
        target_queue.append(&mut source_queue);
        assert_eq!(32, target_queue.layout.align());
        assert_eq!(64, target_queue.end);
        target_queue.apply(&mut world);

        // A target queue that happens to be aligned does not need an alignment command.
        let mut source_queue = CommandQueue::default();
        let mut target_queue = CommandQueue::default();
        target_queue.push(empty_test_command::<u64>());
        source_queue.push(empty_test_command::<()>());
        target_queue.append(&mut source_queue);
        assert_eq!(MIN_COMMAND_QUEUE_ALIGN.get(), target_queue.layout.align());
        assert_eq!(24, target_queue.end);
        target_queue.apply(&mut world);

        // A target queue that is not aligned needs to reserve space for
        // the alignment command and *then* pad to align the source queue.
        let mut source_queue = CommandQueue::default();
        let mut target_queue = CommandQueue::default();
        target_queue.push(empty_test_command::<()>());
        source_queue.push(empty_test_command::<()>());
        target_queue.append(&mut source_queue);
        assert_eq!(MIN_COMMAND_QUEUE_ALIGN.get(), target_queue.layout.align());
        assert_eq!(40, target_queue.end);
        target_queue.apply(&mut world);
    }

    #[test]
    fn world_command_queue_reuse() {
        const SIZE: usize = size_of::<MetaAndCommand<usize>>();
        let mut world = World::new();
        let command_queue = world.command_queue.get_mut();
        // Call `reserve` manually so we can identify the buffers by length
        command_queue.reserve(Layout::new::<[u8; SIZE * 10]>());

        world.commands().queue(test_command::<usize>(|world| {
            let command_queue = world.command_queue.get_mut();
            assert!(!command_queue.owned);
            assert_eq!(SIZE * 10, command_queue.layout.size());
            assert_eq!(SIZE, command_queue.start);

            world.commands().queue(test_command::<usize>(|world| {
                world.commands().queue(empty_test_command::<usize>());
                let command_queue = world.command_queue.get_mut();
                assert!(!command_queue.owned);
                assert_eq!(SIZE * 10, command_queue.layout.size());
                assert_eq!(SIZE * 2, command_queue.start);
                world.commands().queue(empty_test_command::<usize>());
            }));
            world.flush_commands();
            // A borrowed queue was returned to a borrowed queue
            // Continue using the borrowed queue
            let command_queue = world.command_queue.get_mut();
            assert!(!command_queue.owned);
            assert_eq!(SIZE * 10, command_queue.layout.size());
            assert_eq!(SIZE, command_queue.start);

            world.commands().queue(test_command::<usize>(|world| {
                world.commands().queue(empty_test_command::<usize>());
                let command_queue = world.command_queue.get_mut();
                // Force a new queue to be allocated for testing
                command_queue.reserve(Layout::new::<[u8; SIZE * (20 - 3)]>());
                assert!(command_queue.owned);
                assert_eq!(SIZE * 20, command_queue.layout.size());
                assert_eq!(SIZE * 2, command_queue.start);
                world.commands().queue(empty_test_command::<usize>());
            }));
            world.flush_commands();
            // An owned queue was returned to a borrowed queue
            // Use the new owned queue
            let command_queue = world.command_queue.get_mut();
            assert!(command_queue.owned);
            assert_eq!(SIZE * 20, command_queue.layout.size());
            assert_eq!(SIZE, command_queue.start);

            world.commands().queue(test_command::<usize>(|world| {
                world.commands().queue(empty_test_command::<usize>());
                let command_queue = world.command_queue.get_mut();
                assert!(!command_queue.owned);
                assert_eq!(SIZE * 20, command_queue.layout.size());
                assert_eq!(SIZE * 2, command_queue.start);
                world.commands().queue(empty_test_command::<usize>());
            }));
            world.flush_commands();
            // A borrowed queue was returned to an owned queue
            // Continue using the owned queue
            let command_queue = world.command_queue.get_mut();
            assert!(command_queue.owned);
            assert_eq!(SIZE * 20, command_queue.layout.size());
            assert_eq!(SIZE, command_queue.start);

            world.commands().queue(test_command::<usize>(|world| {
                world.commands().queue(empty_test_command::<usize>());
                let command_queue = world.command_queue.get_mut();
                // Force a new queue to be allocated for testing
                command_queue.reserve(Layout::new::<[u8; SIZE * (40 - 3)]>());
                assert!(command_queue.owned);
                assert_eq!(SIZE * 40, command_queue.layout.size());
                assert_eq!(SIZE * 2, command_queue.start);
                world.commands().queue(empty_test_command::<usize>());
            }));
            world.flush_commands();
            // An owned queue was returned to an owned queue queue
            // Use the new owned queue
            let command_queue = world.command_queue.get_mut();
            assert!(command_queue.owned);
            assert_eq!(SIZE * 40, command_queue.layout.size());
            assert_eq!(SIZE, command_queue.start);
        }));
        world.flush_commands();
    }
}

use crate::prelude::*;

#[derive(Default, Component)]
struct Matrix([[f32; 4]; 4]);

#[derive(Default, Component)]
struct Vec3([f32; 3]);

#[unsafe(no_mangle)]
#[inline(never)]
pub fn asm_insert_commands(
    command_queue: &mut CommandQueue,
    world: &mut World,
    entities: &[Entity],
) {
    let mut commands = Commands::new(command_queue, world);
    for entity in entities {
        commands
            .entity(*entity)
            .insert((Matrix::default(), Vec3::default()));
    }
    command_queue.apply(world);
}

const ENTITY_COUNT: usize = 10_000;

#[derive(Component)]
struct A;

#[unsafe(no_mangle)]
#[inline(never)]
pub fn asm_spawn_one_zst(world: &mut World) {
    for _ in 0..ENTITY_COUNT {
        world.spawn(A);
    }
    world.clear_entities();
}

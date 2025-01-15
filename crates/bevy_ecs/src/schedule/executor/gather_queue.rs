use alloc::alloc::{alloc, dealloc};
use core::{
    alloc::Layout,
    iter::from_fn,
    ptr::dangling_mut,
    sync::atomic::{AtomicUsize, Ordering::*},
};

use thread_local::ThreadLocal;

/// A multiple producer single consumer queue where each producer
/// writes to an isolated bounded queue and the consumer scans all of them.
/// It requires no atomic read-modify-write operations.
/// The caller is responsible for ensuring that only one thread reads at a time.
pub struct GatherQueue<T: Send> {
    local_queues: ThreadLocal<AtomicQueue<T>>,
    len: usize,
}

impl<T: Send> GatherQueue<T> {
    /// Creates a new `GatherQueue` whose local queues each have the given capacity.
    pub fn new(len: usize) -> Self {
        Self {
            len,
            local_queues: ThreadLocal::new(),
        }
    }

    /// Attemps to push an item to the local queue for the current thread.
    /// If the local queue if full, returns `Err` with the item.
    pub fn push(&self, value: T) -> Result<(), T> {
        let local_queue = self.local_queues.get_or(|| AtomicQueue::new(self.len));
        // SAFETY: We only call `push` for the local queue of the current thread,
        // so all calls were on this thread.
        unsafe { local_queue.push(value) }
    }

    /// Pops all items from local queues.
    ///
    /// # Safety
    ///
    /// This must be synchronized before or after any calls to `drain` from other threads.
    pub unsafe fn drain(&self) -> impl Iterator<Item = T> + '_ {
        self.local_queues
            .iter()
            // SAFETY: Caller ensures this is synchronized
            .flat_map(|queue| queue.drain())
    }
}

/// A bounded queue that may be written to on one thread and read from
/// on a different thread without using atomic read-modify-write operations.
/// The caller is responsible for ensuring that
/// only one thread reads and only one thread writes.
struct AtomicQueue<T> {
    len: usize,
    ptr: *mut T,
    // TODO: Try putting `head` in a separate cache line
    //  and adding a *copy* owned by `push`
    //  when the queue looks empty, `push` will read the atomic
    //  once to update the copy in case it has advanced
    //  that would keep writes from the reader and writer to separate
    //  cache lines, and mean the writer usually only needs to read its own
    //  hopefully that means it never gets slowed down,
    //  since it can buffer the stores when the reader reads
    // The index of the next item to pop.
    // If this equals `tail`, the queue is empty.
    head: AtomicUsize,
    // The index of the next item to push.
    // If this is one less than `head`, the queue is full.
    tail: AtomicUsize,
}

// SAFETY: Values of `T` are sent through the queue but never shared by reference
unsafe impl<T: Send> Sync for AtomicQueue<T> {}

// SAFETY: This was only non-send due to the use of a raw pointer
unsafe impl<T: Send> Send for AtomicQueue<T> {}

impl<T> AtomicQueue<T> {
    fn new(len: usize) -> Self {
        // Waste one slot to distinguish empty from full queues.
        let len = len + 1;
        let layout = Layout::array::<T>(len).unwrap();
        let ptr = if layout.size() == 0 {
            dangling_mut()
        } else {
            // SAFETY: We checked for zero size
            unsafe { alloc(layout) }.cast()
        };
        Self {
            len,
            ptr,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    fn increment_index(&self, index: usize) -> usize {
        if index == self.len - 1 {
            0
        } else {
            index + 1
        }
    }

    /// Pushes a value to the local queue.
    ///
    /// # Safety
    ///
    /// This must be synchronized before or after any calls to `push` from other threads.
    unsafe fn push(&self, value: T) -> Result<(), T> {
        // This is only written by `push`, which cannot be running concurrently,
        // so this value will not change during this method.
        let tail = self.tail.load(Relaxed);
        // Other threads may increment head, but it will never decrease.
        // So we may incorrectly believe the queue is full,
        // but we will never incorrectly believe it has space.
        let head = self.head.load(Acquire);
        let new_tail = self.increment_index(tail);
        // If the queue is full, don't push.
        // Note that we waste one slot to distinguish empty from full queues.
        if new_tail == head {
            return Err(value);
        }
        // Write to the old tail, then increment the tail.
        // SAFETY: `ptr` points to an array of `T`, and `tail` is in range,
        // so this is a valid place for `value`.
        unsafe { self.ptr.add(tail).write(value) };
        self.tail.store(new_tail, Release);
        Ok(())
    }

    /// Pops a value to the local queue.
    ///
    /// # Safety
    ///
    /// This must be synchronized before or after any calls to `pop` or `drain` from other threads.
    unsafe fn pop(&self) -> Option<T> {
        // Other threads may increment tail, but it will never decrease.
        // So we may incorrectly believe the queue is empty,
        // but we will never incorrectly believe it has elements.
        let tail = self.tail.load(Acquire);
        // This is only written by `pop`, which cannot be running concurrently,
        // so this value will not change during this method.
        let head = self.head.load(Relaxed);
        // If the queue is empty, don't pop.
        if tail == head {
            return None;
        }
        // Read from the old head, then increment the head.
        // SAFETY: `ptr` points to an array of `T`, and `tail` is in range,
        // so this is a valid place for `value`.
        // We increment `head` after reading, so this was only read once.
        // We read an unequal `tail` with `Acquire`, so another thread
        // wrote a valid `T` there and this read happens-after that write.
        let value = unsafe { self.ptr.add(head).read() };
        self.head.store(self.increment_index(head), Release);
        Some(value)
    }

    /// Pops all values in the local queue.
    ///
    /// # Safety
    ///
    /// This must be synchronized before or after any calls to `pop` or `drain` from other threads.
    unsafe fn drain(&self) -> impl Iterator<Item = T> + '_ {
        // SAFETY: Caller ensures this is synchronized
        from_fn(|| unsafe { self.pop() })
    }
}

impl<T> Drop for AtomicQueue<T> {
    fn drop(&mut self) {
        // SAFETY: We have &mut access, so any other calls were synchronized before it.
        for value in unsafe { self.drain() } {
            drop(value);
        }
        let layout = Layout::array::<T>(self.len).unwrap();
        if layout.size() != 0 {
            // SAFETY: We got `ptr` by calling `alloc` with this same layout.
            unsafe { dealloc(self.ptr.cast(), layout) };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::thread::{scope, yield_now};

    use alloc::{vec, vec::Vec};

    use super::*;

    #[test]
    fn drain_returns_items_from_same_thread() {
        let queue = GatherQueue::new(10);
        queue.push(1).unwrap();
        queue.push(2).unwrap();
        queue.push(3).unwrap();
        let result = unsafe { queue.drain() }.collect::<Vec<_>>();
        assert_eq!(result, vec![1, 2, 3]);
    }

    #[test]
    fn push_fails_on_full_queue() {
        let queue = GatherQueue::new(2);
        queue.push(1).unwrap();
        queue.push(2).unwrap();
        assert_eq!(3, queue.push(3).unwrap_err());
        assert_eq!(4, queue.push(4).unwrap_err());
    }

    #[test]
    fn alternating_push_and_pop_wraps_indexes() {
        let queue = GatherQueue::new(1);
        for i in 0..100 {
            queue.push(i).unwrap();
            let result = unsafe { queue.drain() }.collect::<Vec<_>>();
            assert_eq!(vec![i], result);
        }
    }

    #[test]
    fn drain_aggregates_items_from_other_threads() {
        let queue = GatherQueue::new(10);
        scope(|scope| {
            let queue = &queue;
            for i in 0..10 {
                scope.spawn(move || {
                    queue.push(i).unwrap();
                });
            }
        });
        let mut result = unsafe { queue.drain() }.collect::<Vec<_>>();
        result.sort();
        let expected = (0..10).collect::<Vec<_>>();
        assert_eq!(expected, result);
    }

    #[test]
    fn concurrent_push_and_drain() {
        let queue = GatherQueue::new(10);
        let mut result = Vec::new();
        let threads = 10;
        let items = 100;
        scope(|scope| {
            let queue = &queue;
            scope.spawn(|| {
                while result.len() < threads * items {
                    // SAFETY: We only call `drain` on this thread
                    result.extend(unsafe { queue.drain() });
                    yield_now();
                }
            });
            for i in 0..threads {
                scope.spawn(move || {
                    for j in 0..items {
                        while queue.push(i * items + j).is_err() {
                            yield_now();
                        }
                    }
                });
            }
        });
        result.sort();
        let expected = (0..(threads * items)).collect::<Vec<_>>();
        assert_eq!(expected, result);
    }
}

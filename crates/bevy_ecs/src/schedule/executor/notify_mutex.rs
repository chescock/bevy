use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    mem::ManuallyDrop,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicUsize, Ordering},
};

/// A mutex that allows the owner to be notified to run again.
#[derive(Default)]
pub struct NotifyMutex<T> {
    // The number of notifications since the mutex was first locked.
    // The mutex is unlocked when this is zero and locked when it is nonzero.
    counter: AtomicUsize,
    value: UnsafeCell<T>,
}

impl<T> NotifyMutex<T> {
    /// Creates a new unlocked `NotifyMutex` wrapping the given value.
    pub const fn new(value: T) -> Self {
        Self {
            counter: AtomicUsize::new(0),
            value: UnsafeCell::new(value),
        }
    }

    /// Attempts to acquire the lock.
    /// Returns `None` if the lock was held by another thread.
    pub fn try_lock(&self) -> Option<NotifyMutexGuard<T>> {
        // Load with `Acquire` so that writes to `value` are visible if we take the lock.
        self.counter
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| NotifyMutexGuard::new(self, 1))
    }

    /// Attempts to acquire the lock, and notifies the lock owner to run again if that fails.
    /// If the lock was held by another thread, this returns `None`
    /// and ensures that the next call to `NotifyMutexGuard::try_unlock()` fails.
    ///
    /// # Safety
    ///
    /// This must not be called more than `usize::MAX` times before being unlocked,
    /// or the internal counter will overflow.
    pub unsafe fn lock_or_notify(&self) -> Option<NotifyMutexGuard<T>> {
        // Unconditionally increment `counter`.
        // If it was unlocked, it will increment from 0 to 1 and we acquire the lock.
        // Otherwise, it will increment the counter above what the lock owner expects,
        // causing the `compare_exchange` in `try_unlock` to fail.
        // Load with `Acquire` so that writes to `value` are visible if we take the lock,
        // and store with `Release` so that the writes being notified are visible to the lock if we do not.
        let lock_taken = self.counter.fetch_add(1, Ordering::AcqRel) == 0;
        lock_taken.then(|| NotifyMutexGuard::new(self, 1))
    }

    /// Returns a mutable reference to the underlying data.
    pub fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }
}

// SAFETY: The locking strategy ensures that the inner value
// is only accessed on one thread at a time.
unsafe impl<T: Send> Sync for NotifyMutex<T> {}

/// A scoped lock for a `NotifyMutex`.
/// This must not be dropped normally, but must be consumed with `try_unlock`
/// in case a notification arrived while it was locked.
pub struct NotifyMutexGuard<'a, T> {
    mutex: &'a NotifyMutex<T>,
    /// The value of `mutex.counter` when the guard was initialized.
    counter: usize,
    // Ensure that `NotifyMutexGuard` is not `Sync` if `T` is not.
    marker: PhantomData<&'a mut T>,
}

impl<'a, T> NotifyMutexGuard<'a, T> {
    /// Creates a new `NotifyMutexGuard` for the given mutex.
    /// `counter` should equal the current value of `mutex.counter`.
    fn new(mutex: &'a NotifyMutex<T>, counter: usize) -> Self {
        Self {
            mutex,
            counter,
            marker: PhantomData,
        }
    }

    /// Attempts to unlock the `NotifyMutex`.
    /// If `NotifyMutex::lock_or_notify()` has been called since this was created,
    /// this will return `Err` with a new `NotifyMutexGuard`.
    pub fn try_unlock(self) -> Result<(), Self> {
        // Ensure that the panicking `Drop` impl is not called.
        let this = ManuallyDrop::new(self);
        // If the counter matches our cached value, there were no new notifications
        // and we can set the counter to zero and unlock.
        // If the counter has changed, then `lock_or_notify` was called while we held
        // the lock.  Return a new `NotifyMutexGuard` with the new value of the counter.
        // Store with `Release` on success so that writes to `value`
        // are visible to the next thread taking the lock.
        // Load with `Acquire` on failure so that the writes being notified
        // are visible to the current thread.
        this.mutex
            .counter
            .compare_exchange(this.counter, 0, Ordering::Release, Ordering::Acquire)
            .map(|_| {})
            .map_err(|counter| Self::new(this.mutex, counter))
    }
}

impl<'a, T> Deref for NotifyMutexGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY:
        // Only one `NotifyMutexGuard` can exist, so we have exclusive access.
        // We unlock with `Release` stores to `counter` and lock with `Acquire` reads,
        // so all changes to the cell are synchronized.
        // This is only on another thread if `NotifyMutex: Sync`,
        // which requires `T: Send`.
        unsafe { &*self.mutex.value.get() }
    }
}

impl<'a, T> DerefMut for NotifyMutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY:
        // Only one `NotifyMutexGuard` can exist, so we have exclusive access.
        // We unlock with `Release` stores to `counter` and lock with `Acquire` reads,
        // so all changes to the cell are synchronized.
        // This is only on another thread if `NotifyMutex: Sync`,
        // which requires `T: Send`.
        unsafe { &mut *self.mutex.value.get() }
    }
}

impl<'a, T> Drop for NotifyMutexGuard<'a, T> {
    fn drop(&mut self) {
        panic!("`NotifyMutexGuard` must be unlocked by calling `try_unlock` to avoid lost notifications.");
    }
}

#[cfg(test)]
mod tests {
    use super::NotifyMutex;

    #[test]
    fn fails_when_locked() {
        let mutex = NotifyMutex::new(());
        let guard = mutex.try_lock().unwrap();
        assert!(mutex.try_lock().is_none());
        assert!(guard.try_unlock().is_ok());
    }

    #[test]
    fn notify_takes_lock() {
        let mutex = NotifyMutex::new(());
        // SAFETY: We only call this once
        let guard = unsafe { mutex.lock_or_notify() }.unwrap();
        assert!(guard.try_unlock().is_ok());
    }

    #[test]
    fn notify_makes_unlock_fail() {
        let mutex = NotifyMutex::new(());
        let guard = mutex.try_lock().unwrap();
        // SAFETY: We only call this three times
        assert!(unsafe { mutex.lock_or_notify() }.is_none());
        // SAFETY: We only call this three times
        assert!(unsafe { mutex.lock_or_notify() }.is_none());
        let guard = guard.try_unlock().unwrap_err();
        // SAFETY: We only call this three times
        assert!(unsafe { mutex.lock_or_notify() }.is_none());
        let guard = guard.try_unlock().unwrap_err();
        assert!(guard.try_unlock().is_ok());
    }
}

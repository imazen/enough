pub(crate) struct Mutex<T>(std::sync::Mutex<T>);
impl<T> Mutex<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self(std::sync::Mutex::new(value))
    }
    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, T> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub(crate) fn try_lock(&self) -> Option<std::sync::MutexGuard<'_, T>> {
        match self.0.try_lock() {
            Ok(guard) => Some(guard),
            Err(std::sync::TryLockError::Poisoned(error)) => Some(error.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }
}

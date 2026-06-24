#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialog<T> {
    pub id: String,
    pub payload: T,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DialogStack<T> {
    stack: Vec<Dialog<T>>,
}

impl<T> Dialog<T> {
    #[must_use]
    pub fn new(id: impl Into<String>, payload: T) -> Self {
        Self {
            id: id.into(),
            payload,
        }
    }
}

impl<T> DialogStack<T> {
    #[must_use]
    pub const fn new() -> Self {
        Self { stack: Vec::new() }
    }

    pub fn push(&mut self, dialog: Dialog<T>) {
        self.stack.push(dialog);
    }

    pub fn pop(&mut self) -> Option<Dialog<T>> {
        self.stack.pop()
    }

    #[must_use]
    pub fn current(&self) -> Option<&Dialog<T>> {
        self.stack.last()
    }

    pub fn clear(&mut self) {
        self.stack.clear();
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.stack.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }
}

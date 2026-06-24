#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScrollState {
    pub offset: usize,
    pub content_height: usize,
    pub viewport_height: usize,
}

impl ScrollState {
    #[must_use]
    pub fn new(content_height: usize, viewport_height: usize) -> Self {
        Self {
            offset: 0,
            content_height,
            viewport_height,
        }
    }

    #[must_use]
    pub fn max_offset(&self) -> usize {
        self.content_height.saturating_sub(self.viewport_height)
    }

    pub fn set_content_height(&mut self, content_height: usize) {
        self.content_height = content_height;
        self.clamp();
    }

    pub fn set_viewport_height(&mut self, viewport_height: usize) {
        self.viewport_height = viewport_height;
        self.clamp();
    }

    pub fn scroll_by(&mut self, delta: isize) {
        if delta >= 0 {
            self.offset = self.offset.saturating_add(delta.unsigned_abs());
        } else {
            self.offset = self.offset.saturating_sub(delta.unsigned_abs());
        }
        self.clamp();
    }

    pub fn scroll_to(&mut self, offset: usize) {
        self.offset = offset;
        self.clamp();
    }

    pub fn page_down(&mut self) {
        self.scroll_by(usize_to_isize(self.viewport_height));
    }

    pub fn page_up(&mut self) {
        self.scroll_by(-usize_to_isize(self.viewport_height));
    }

    pub fn half_page_down(&mut self) {
        self.scroll_by(usize_to_isize(self.viewport_height / 2));
    }

    pub fn half_page_up(&mut self) {
        self.scroll_by(-usize_to_isize(self.viewport_height / 2));
    }

    pub fn to_top(&mut self) {
        self.offset = 0;
    }

    pub fn to_bottom(&mut self) {
        self.offset = self.max_offset();
    }

    pub fn sticky_bottom(&mut self, old_content_height: usize, new_content_height: usize) {
        let was_bottom = self.offset >= old_content_height.saturating_sub(self.viewport_height);
        self.content_height = new_content_height;
        if was_bottom {
            self.to_bottom();
            return;
        }
        self.clamp();
    }

    fn clamp(&mut self) {
        self.offset = self.offset.min(self.max_offset());
    }
}

fn usize_to_isize(value: usize) -> isize {
    match isize::try_from(value) {
        Ok(converted) => converted,
        Err(_) => isize::MAX,
    }
}

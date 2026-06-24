use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogSelectItem<T> {
    pub title: String,
    pub value: T,
    pub description: Option<String>,
    pub footer: Option<String>,
    pub category: Option<String>,
    pub disabled: bool,
    pub unfiltered_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogSelect<T> {
    items: Vec<DialogSelectItem<T>>,
    filter: String,
    selected: usize,
}

impl<T> DialogSelectItem<T> {
    #[must_use]
    pub fn new(title: impl Into<String>, value: T) -> Self {
        Self {
            title: title.into(),
            value,
            description: None,
            footer: None,
            category: None,
            disabled: false,
            unfiltered_only: false,
        }
    }

    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    #[must_use]
    pub fn with_footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    #[must_use]
    pub fn with_category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
        self
    }

    #[must_use]
    pub const fn disabled(mut self) -> Self {
        self.disabled = true;
        self
    }

    #[must_use]
    pub const fn unfiltered_only(mut self) -> Self {
        self.unfiltered_only = true;
        self
    }
}

impl<T> DialogSelect<T> {
    #[must_use]
    pub fn new(items: Vec<DialogSelectItem<T>>) -> Self {
        Self {
            items,
            filter: String::new(),
            selected: 0,
        }
    }

    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    #[must_use]
    pub const fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn set_selected(&mut self, index: usize) {
        self.selected = index;
        self.clamp_selection();
    }

    pub fn set_filter(&mut self, filter: impl Into<String>) {
        self.filter = filter.into();
        self.selected = 0;
        self.clamp_selection();
    }

    pub fn move_down(&mut self) {
        let total = self.filtered_len();
        if total == 0 {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected + 1) % total;
    }

    pub fn move_up(&mut self) {
        let total = self.filtered_len();
        if total == 0 {
            self.selected = 0;
            return;
        }
        self.selected = if self.selected == 0 {
            total - 1
        } else {
            self.selected - 1
        };
    }

    #[must_use]
    pub fn select(&self) -> Option<&T> {
        self.filtered_items()
            .get(self.selected)
            .map(|item| &item.value)
    }

    #[must_use]
    pub fn filtered_items(&self) -> Vec<&DialogSelectItem<T>> {
        let enabled = self.items.iter().filter(|item| !item.disabled);
        if self.filter.trim().is_empty() {
            return enabled.collect();
        }
        let needle = self.filter.to_lowercase();
        let matcher = SkimMatcherV2::default().ignore_case();
        let mut scored = enabled
            .filter(|item| !item.unfiltered_only)
            .filter_map(|item| fuzzy_score(&matcher, &needle, item).map(|score| (score, item)))
            .collect::<Vec<_>>();
        scored.sort_by(|left, right| right.0.cmp(&left.0));
        scored.into_iter().map(|(_, item)| item).collect()
    }

    fn filtered_len(&self) -> usize {
        self.filtered_items().len()
    }

    fn clamp_selection(&mut self) {
        let total = self.filtered_len();
        if total == 0 {
            self.selected = 0;
            return;
        }
        self.selected = self.selected.min(total - 1);
    }
}

fn fuzzy_score<T>(
    matcher: &SkimMatcherV2,
    needle: &str,
    item: &DialogSelectItem<T>,
) -> Option<i64> {
    let title = match matcher.fuzzy_match(&item.title, needle) {
        Some(score) => score * 2,
        None => 0,
    };
    let category = item
        .category
        .as_deref()
        .and_then(|category| matcher.fuzzy_match(category, needle))
        .map_or(0, |score| score);
    let score = title + category;
    (score > 0).then_some(score)
}

use serde_json::Value;

use super::model::{
    custom_enabled, multiple, option_label, options, question_at, questions, single, tabs,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionAction {
    None,
    Reply { answers: Vec<Vec<String>> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionState {
    request_id: String,
    tab: usize,
    answers: Vec<Vec<String>>,
    custom: Vec<String>,
    selected: usize,
    editing: bool,
    edit: String,
}

impl QuestionState {
    #[must_use]
    pub fn new(request_id: &str) -> Self {
        Self {
            request_id: request_id.to_owned(),
            tab: 0,
            answers: Vec::new(),
            custom: Vec::new(),
            selected: 0,
            editing: false,
            edit: String::new(),
        }
    }

    pub fn sync(&mut self, request_id: &str) {
        if self.request_id != request_id {
            *self = Self::new(request_id);
        }
    }

    #[must_use]
    pub const fn tab(&self) -> usize {
        self.tab
    }

    #[must_use]
    pub const fn editing(&self) -> bool {
        self.editing
    }

    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    #[must_use]
    pub fn edit(&self) -> &str {
        &self.edit
    }

    #[must_use]
    pub fn custom_input(&self) -> &str {
        self.custom.get(self.tab).map_or("", String::as_str)
    }

    #[must_use]
    pub fn confirm(&self, request: &Value) -> bool {
        !single(request) && self.tab == questions(request).len()
    }

    #[must_use]
    pub fn selectable_count(&self, request: &Value) -> usize {
        question_at(request, self.tab)
            .map(|question| options(question).len() + usize::from(custom_enabled(question)))
            .unwrap_or(0)
    }

    pub const fn set_selected(&mut self, selected: usize) {
        self.selected = selected;
    }

    pub fn push_custom_char(&mut self, ch: char) {
        self.edit.push(ch);
    }

    pub fn pop_custom_char(&mut self) {
        self.edit.pop();
    }

    pub fn cancel_edit(&mut self) {
        self.editing = false;
        self.edit.clear();
    }

    #[must_use]
    pub fn answers(&self, count: usize) -> Vec<Vec<String>> {
        (0..count)
            .map(|index| self.answers.get(index).cloned().unwrap_or_default())
            .collect()
    }

    #[must_use]
    pub fn answer_at(&self, index: usize) -> Option<Vec<String>> {
        self.answers
            .get(index)
            .filter(|answers| !answers.is_empty())
            .cloned()
    }

    #[must_use]
    pub fn is_picked(&self, label: &str) -> bool {
        self.answers
            .get(self.tab)
            .is_some_and(|answers| answers.iter().any(|answer| answer == label))
    }

    #[must_use]
    pub fn custom_picked(&self) -> bool {
        let value = self.custom_input();
        !value.is_empty() && self.is_picked(value)
    }

    pub fn next_tab(&mut self, request: &Value) {
        self.select_tab(request, 1);
    }

    pub fn previous_tab(&mut self, request: &Value) {
        self.select_tab(request, tabs(request).saturating_sub(1));
    }

    pub fn move_selected(&mut self, request: &Value, direction: isize) {
        let total = self.selectable_count(request);
        if total != 0 {
            self.selected =
                (self.selected as isize + direction).rem_euclid(total as isize) as usize;
        }
    }

    pub fn select(&mut self, request: &Value) -> QuestionAction {
        let Some(question) = question_at(request, self.tab) else {
            return QuestionAction::None;
        };
        if self.custom_row(request) {
            if multiple(question) && self.custom_picked() {
                self.toggle(self.custom_input().to_owned());
                return QuestionAction::None;
            }
            self.editing = true;
            self.edit = self.custom_input().to_owned();
            return QuestionAction::None;
        }
        let Some(answer) = option_label(question, self.selected) else {
            return QuestionAction::None;
        };
        if multiple(question) {
            self.toggle(answer.to_owned());
            return QuestionAction::None;
        }
        self.pick(request, answer, false)
    }

    pub fn save_custom(&mut self, request: &Value) -> QuestionAction {
        let Some(question) = question_at(request, self.tab) else {
            return QuestionAction::None;
        };
        let value = self.edit.trim().to_owned();
        let prev = self.custom.get(self.tab).cloned().unwrap_or_default();
        if value.is_empty() {
            self.store_custom(String::new());
            if !prev.is_empty() {
                self.remove_answer(&prev);
            }
            self.cancel_edit();
            return QuestionAction::None;
        }
        if multiple(question) {
            if !prev.is_empty() {
                self.remove_answer(&prev);
            }
            self.store_custom(value.clone());
            self.add_answer(value);
            self.cancel_edit();
            return QuestionAction::None;
        }
        self.cancel_edit();
        self.pick(request, &value, true)
    }

    #[must_use]
    pub fn submit(&self, request: &Value) -> QuestionAction {
        QuestionAction::Reply {
            answers: self.answers(questions(request).len()),
        }
    }

    fn select_tab(&mut self, request: &Value, step: usize) {
        let count = tabs(request);
        if count != 0 {
            self.tab = (self.tab + step) % count;
            self.selected = 0;
            self.cancel_edit();
        }
    }

    fn custom_row(&self, request: &Value) -> bool {
        question_at(request, self.tab).is_some_and(|question| {
            custom_enabled(question) && self.selected == options(question).len()
        })
    }

    fn pick(&mut self, request: &Value, answer: &str, custom: bool) -> QuestionAction {
        self.store_answers(vec![answer.to_owned()]);
        if custom {
            self.store_custom(answer.to_owned());
        }
        if single(request) {
            return QuestionAction::Reply {
                answers: vec![vec![answer.to_owned()]],
            };
        }
        self.next_tab(request);
        QuestionAction::None
    }

    fn toggle(&mut self, answer: String) {
        if self.current_answers().iter().any(|item| item == &answer) {
            self.remove_answer(&answer);
        } else {
            self.add_answer(answer);
        }
    }

    fn add_answer(&mut self, answer: String) {
        let mut list = self.current_answers();
        if !list.iter().any(|item| item == &answer) {
            list.push(answer);
        }
        self.store_answers(list);
    }

    fn remove_answer(&mut self, answer: &str) {
        let mut list = self.current_answers();
        list.retain(|item| item != answer);
        self.store_answers(list);
    }

    fn current_answers(&self) -> Vec<String> {
        self.answers.get(self.tab).cloned().unwrap_or_default()
    }

    fn store_answers(&mut self, answers: Vec<String>) {
        if self.answers.len() <= self.tab {
            self.answers.resize_with(self.tab + 1, Vec::new);
        }
        self.answers[self.tab] = answers;
    }

    fn store_custom(&mut self, answer: String) {
        if self.custom.len() <= self.tab {
            self.custom.resize_with(self.tab + 1, String::new);
        }
        self.custom[self.tab] = answer;
    }
}

impl Default for QuestionState {
    fn default() -> Self {
        Self::new("")
    }
}

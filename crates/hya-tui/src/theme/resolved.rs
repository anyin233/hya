use crate::contracts::Rgba;

macro_rules! theme_color_fields {
    ($macro:ident) => {
        $macro! {
            primary => "primary",
            secondary => "secondary",
            accent => "accent",
            error => "error",
            warning => "warning",
            success => "success",
            info => "info",
            text => "text",
            text_muted => "textMuted",
            selected_list_item_text => "selectedListItemText",
            background => "background",
            background_panel => "backgroundPanel",
            background_element => "backgroundElement",
            background_menu => "backgroundMenu",
            border => "border",
            border_active => "borderActive",
            border_subtle => "borderSubtle",
            diff_added => "diffAdded",
            diff_removed => "diffRemoved",
            diff_context => "diffContext",
            diff_hunk_header => "diffHunkHeader",
            diff_highlight_added => "diffHighlightAdded",
            diff_highlight_removed => "diffHighlightRemoved",
            diff_added_bg => "diffAddedBg",
            diff_removed_bg => "diffRemovedBg",
            diff_context_bg => "diffContextBg",
            diff_line_number => "diffLineNumber",
            diff_added_line_number_bg => "diffAddedLineNumberBg",
            diff_removed_line_number_bg => "diffRemovedLineNumberBg",
            markdown_text => "markdownText",
            markdown_heading => "markdownHeading",
            markdown_link => "markdownLink",
            markdown_link_text => "markdownLinkText",
            markdown_code => "markdownCode",
            markdown_block_quote => "markdownBlockQuote",
            markdown_emph => "markdownEmph",
            markdown_strong => "markdownStrong",
            markdown_horizontal_rule => "markdownHorizontalRule",
            markdown_list_item => "markdownListItem",
            markdown_list_enumeration => "markdownListEnumeration",
            markdown_image => "markdownImage",
            markdown_image_text => "markdownImageText",
            markdown_code_block => "markdownCodeBlock",
            syntax_comment => "syntaxComment",
            syntax_keyword => "syntaxKeyword",
            syntax_function => "syntaxFunction",
            syntax_variable => "syntaxVariable",
            syntax_string => "syntaxString",
            syntax_number => "syntaxNumber",
            syntax_type => "syntaxType",
            syntax_operator => "syntaxOperator",
            syntax_punctuation => "syntaxPunctuation",
        }
    };
}

macro_rules! define_resolved_theme {
    ($($field:ident => $key:literal,)+) => {
        #[derive(Debug, Clone, PartialEq)]
        pub struct ResolvedTheme {
            $(pub $field: Rgba,)+
            pub thinking_opacity: f64,
            pub has_selected_list_item_text: bool,
        }

        pub const THEME_COLOR_KEYS: &[&str] = &[$($key,)+];

        impl ResolvedTheme {
            pub(crate) fn try_from_colors<E>(
                mut color: impl FnMut(&'static str) -> Result<Rgba, E>,
                thinking_opacity: f64,
                has_selected_list_item_text: bool,
            ) -> Result<Self, E> {
                Ok(Self {
                    $($field: color($key)?,)+
                    thinking_opacity,
                    has_selected_list_item_text,
                })
            }

            #[must_use]
            pub fn color(&self, key: &str) -> Option<Rgba> {
                match key {
                    $($key => Some(self.$field),)+
                    _ => None,
                }
            }
        }
    };
}

theme_color_fields!(define_resolved_theme);

pub(crate) const SELECTED_LIST_ITEM_TEXT_KEY: &str = "selectedListItemText";
pub(crate) const BACKGROUND_MENU_KEY: &str = "backgroundMenu";
pub(crate) const BACKGROUND_KEY: &str = "background";
pub(crate) const BACKGROUND_ELEMENT_KEY: &str = "backgroundElement";
pub(crate) const PRIMARY_KEY: &str = "primary";
pub(crate) const THINKING_OPACITY_KEY: &str = "thinkingOpacity";

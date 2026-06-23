use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(in crate::widgets) fn ellipsize(s: &str, max: usize) -> String {
    let cleaned = s.replace('\n', " ");
    if UnicodeWidthStr::width(cleaned.as_str()) <= max {
        return cleaned;
    }
    if max == 0 {
        return String::new();
    }

    let limit = max.saturating_sub(1);
    let mut width = 0;
    let mut head = String::new();
    for ch in cleaned.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > limit {
            break;
        }
        head.push(ch);
        width += ch_width;
    }
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthStr;

    use super::ellipsize;

    #[test]
    fn ellipsize_limits_display_width_for_cjk_text() {
        // Given: a wide-character permission/detail label and a narrow terminal budget.
        let text = "权限请求需要读取文件";

        // When: the overlay ellipsizes it for a seven-cell slot.
        let rendered = ellipsize(text, 7);

        // Then: the visible result fits the cell budget and still marks truncation.
        assert!(
            UnicodeWidthStr::width(rendered.as_str()) <= 7,
            "ellipsized CJK text should fit the display-cell budget: {rendered}"
        );
        assert!(
            rendered.ends_with('…'),
            "truncated CJK text should keep an ellipsis marker: {rendered}"
        );
    }
}

//! Pure UTF-16 edit planning shared by JNI and host-side regression tests.
use serde::{Deserialize, Serialize};
use std::ops::Range;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct InputSnapshot {
    pub text: String,
    /// Absolute UTF-16 origin of the mirrored editable window.
    pub offset: usize,
    pub selection: Range<usize>,
    pub composing: Option<Range<usize>>,
    pub enabled: bool,
}
impl InputSnapshot {
    pub fn end(&self) -> usize {
        self.offset + self.text.encode_utf16().count()
    }
    pub fn clamp(&self, range: Range<usize>) -> Range<usize> {
        let a = range.start.clamp(self.offset, self.end());
        let b = range.end.clamp(a, self.end());
        self.boundary(a, false)..self.boundary(b, true)
    }
    pub fn replacement(&self) -> Range<usize> {
        self.clamp(self.composing.clone().unwrap_or(self.selection.clone()))
    }
    pub fn boundary(&self, absolute: usize, forward: bool) -> usize {
        let target = absolute
            .saturating_sub(self.offset)
            .min(self.end() - self.offset);
        let mut n = 0;
        for ch in self.text.chars() {
            let next = n + ch.len_utf16();
            if target < next {
                return self.offset + if target == n || !forward { n } else { next };
            }
            n = next;
        }
        self.end()
    }
    /// Android's cursor parameter is relative to insertion end for positive values,
    /// and insertion start otherwise (InputConnection.commitText contract).
    pub fn cursor_after(&self, replacement: Range<usize>, text: &str, cursor: i32) -> usize {
        let length = text.encode_utf16().count();
        let base = if cursor > 0 {
            replacement.start + length
        } else {
            replacement.start
        };
        let delta = if cursor > 0 {
            cursor as i64 - 1
        } else {
            cursor as i64
        };
        let cursor = (base as i64 + delta).clamp(
            self.offset as i64,
            (self.end() - replacement.len() + length) as i64,
        ) as usize;
        let edited = Self {
            text: format!(
                "{}{}{}",
                self.slice(self.offset..replacement.start),
                text,
                self.slice(replacement.end..self.end())
            ),
            offset: self.offset,
            ..Default::default()
        };
        edited.boundary(cursor, false)
    }
    /// Android excludes the union of selection and composing text from deletion.
    /// Counts are Unicode scalars for codepoint deletion, UTF-16 units otherwise.
    /// Returns after first, before second so the second edit's indices stay valid.
    pub fn surrounding_deletions(
        &self,
        before: usize,
        after: usize,
        codepoints: bool,
    ) -> [Range<usize>; 2] {
        let mut selected = self.clamp(self.selection.clone());
        if let Some(marked) = &self.composing {
            let marked = self.clamp(marked.clone());
            selected.start = selected.start.min(marked.start);
            selected.end = selected.end.max(marked.end);
        }
        let (start, end) = if codepoints {
            let before_text = self.slice(self.offset..selected.start);
            let after_text = self.slice(selected.end..self.end());
            let a = before_text
                .chars()
                .rev()
                .take(before)
                .map(char::len_utf16)
                .sum::<usize>();
            let b = after_text
                .chars()
                .take(after)
                .map(char::len_utf16)
                .sum::<usize>();
            (selected.start - a, selected.end + b)
        } else {
            (
                self.boundary(
                    selected.start.saturating_sub(before).max(self.offset),
                    false,
                ),
                self.boundary(selected.end.saturating_add(after).min(self.end()), true),
            )
        };
        [selected.end..end, start..selected.start]
    }
    pub fn slice(&self, range: Range<usize>) -> String {
        let r = self.clamp(range);
        String::from_utf16_lossy(
            &self
                .text
                .encode_utf16()
                .skip(r.start - self.offset)
                .take(r.len())
                .collect::<Vec<_>>(),
        )
    }
}
#[cfg(test)]
mod tests {
    use super::InputSnapshot;
    fn state(s: &str, selection: std::ops::Range<usize>) -> InputSnapshot {
        InputSnapshot {
            text: s.into(),
            selection,
            enabled: true,
            ..Default::default()
        }
    }
    #[test]
    fn cursor_is_relative_to_inserted_end_or_start() {
        let s = state("abcdef", 2..4);
        assert_eq!(s.cursor_after(2..4, "ع😀", 1), 5);
        assert_eq!(s.cursor_after(2..4, "ع😀", 0), 2);
        assert_eq!(s.cursor_after(2..4, "ع😀", -1), 1);
        assert_eq!(s.cursor_after(2..4, "ع😀", 2), 6);
    }
    #[test]
    fn delete_both_sides_preserves_selection_and_surrogates() {
        let s = state("ا😀BC🦀ع", 3..5);
        assert_eq!(s.surrounding_deletions(1, 1, false), [5..7, 1..3]);
        assert_eq!(s.surrounding_deletions(1, 1, true), [5..7, 1..3]);
        assert_eq!(s.surrounding_deletions(2, 2, true), [5..8, 0..3]);
    }
    #[test]
    fn clamps_to_editable_origin_and_marks_replace_selection() {
        let s = InputSnapshot {
            offset: 100,
            text: "أَهْلاً 😀".into(),
            selection: 102..102,
            composing: Some(100..103),
            enabled: true,
        };
        assert_eq!(s.replacement(), 100..103);
        assert_eq!(
            s.surrounding_deletions(999, 999, false),
            [103..s.end(), 100..100]
        );
        assert_eq!(s.slice(0..101), "أ");
    }
    #[test]
    fn composing_text_is_excluded_from_surrounding_deletion() {
        let mut s = state("x😀سلام🦀y", 5..5);
        s.composing = Some(3..7);
        assert_eq!(s.surrounding_deletions(1, 1, true), [7..9, 1..3]);
        assert_eq!(s.surrounding_deletions(0, 0, false), [7..7, 3..3]);
    }
    #[test]
    fn commit_cursor_never_splits_a_surrogate_pair() {
        let s = state("a😀b", 1..1);
        assert_eq!(s.cursor_after(1..1, "🦀", 0), 1);
        assert_eq!(s.cursor_after(1..1, "🦀", -1), 0);
        assert_eq!(s.cursor_after(1..1, "🦀", 2), 3);
    }
}

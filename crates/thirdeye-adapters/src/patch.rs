//! Unified-diff hunk headers → the line range an edit touched.
//!
//! Shared by every adapter whose agent reports an edit as a patch — OpenCode
//! and pi both do. Only the `@@ -a,b +c,d @@` headers are read: the diff body
//! is file content, and is never kept.

use thirdeye_core::LineRange;

/// The span of every hunk in `patches`, in new-file coordinates — where to
/// open the human. `None` when no header parses.
pub fn span<'a>(patches: impl Iterator<Item = &'a str>) -> Option<LineRange> {
    let mut spans = patches.flat_map(str::lines).filter_map(hunk_span);
    let (mut start, mut end) = spans.next()?;
    for (s, e) in spans {
        start = start.min(s);
        end = end.max(e);
    }
    Some(LineRange::new(start, end))
}

/// `@@ -1,3 +10,4 @@` → `(10, 14)`. A missing count means one line.
fn hunk_span(line: &str) -> Option<(u32, u32)> {
    let new = line
        .strip_prefix("@@ ")?
        .split_whitespace()
        .nth(1)?
        .strip_prefix('+')?;
    let (start, count) = match new.split_once(',') {
        Some((s, c)) => (s.parse::<u32>().ok()?, c.parse::<u32>().ok()?),
        None => (new.parse::<u32>().ok()?, 1),
    };
    // Line 0 does not exist — a deleted file's hunk is `+0,0` — and a
    // corrupt header must not overflow.
    (start > 0).then(|| (start, start.saturating_add(count)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nonsense_hunk_header_cannot_overflow_or_name_line_zero() {
        // A deleted file's hunk is `+0,0`; a corrupt one can be anything.
        assert_eq!(hunk_span("@@ -1,3 +0,0 @@"), None);
        assert_eq!(
            hunk_span("@@ -1 +4294967295,5 @@"),
            Some((4294967295, u32::MAX))
        );
    }

    #[test]
    fn every_hunk_of_every_patch_widens_the_span() {
        let a = "--- f\n+++ f\n@@ -1,3 +1,3 @@\n x\n";
        let b = "@@ -10,2 +10,4 @@\n y\n";
        assert_eq!(span([a, b].into_iter()), Some(LineRange::new(1, 14)));
        assert_eq!(span(["no hunks here"].into_iter()), None);
    }
}

//! Fold model for the Inspect modal's JSON viewer.
//!
//! The Inspect modal receives machine-formatted, pretty-printed JSON as a
//! `Vec<String>` (2-space indent, deterministic from
//! `serde_json::to_string_pretty`). Rather than re-model the document as a
//! `serde_json::Value` tree, collapsible regions are derived directly from
//! that text by indentation/brace matching. This keeps the existing
//! `colorize_json_line` renderer and the manifest/config concatenation
//! untouched.
//!
//! A **fold opener** is any line whose trimmed text ends in `{` or `[`
//! (e.g. `"config": {`). Its block runs to the matching closer — the line
//! whose trimmed text starts with `}` or `]` at the same nesting level.
//! Blank lines and the `── config ──` separator are neither, and render as
//! plain leaves.

/// Per-line fold metadata, one entry per source line, index-aligned with
/// the modal's `lines`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowMeta {
    /// Leading-space count of the line.
    pub indent: u16,
    /// True if this line opens a collapsible block.
    pub opener: bool,
    /// For an opener, the index of its matching closer line. For any other
    /// line, its own index (so `close_idx >= self index` always holds).
    pub close_idx: usize,
}

/// The bracket that closes a given opener line, or `None` if not an opener.
pub fn close_bracket(line: &str) -> Option<char> {
    match line.trim_end().chars().last() {
        Some('{') => Some('}'),
        Some('[') => Some(']'),
        _ => None,
    }
}

fn is_opener(line: &str) -> bool {
    close_bracket(line).is_some()
}

fn is_closer(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with('}') || t.starts_with(']')
}

fn indent_of(line: &str) -> u16 {
    line.chars().take_while(|c| *c == ' ').count() as u16
}

/// Build fold metadata for every line via a brace-matching stack.
///
/// Well-formed pretty JSON nests cleanly, so a closer always matches the
/// most-recently-seen still-open opener. Multiple top-level documents
/// (manifest then config) are handled naturally: the stack empties after
/// each document's final closer.
pub fn build_rows(lines: &[String]) -> Vec<RowMeta> {
    let mut rows: Vec<RowMeta> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| RowMeta {
            indent: indent_of(l),
            opener: false,
            close_idx: i,
        })
        .collect();

    let mut stack: Vec<usize> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        // A closer resolves the innermost open block first…
        if is_closer(line)
            && let Some(open_i) = stack.pop()
        {
            rows[open_i].close_idx = i;
        }
        // …then this line may itself open a new block.
        if is_opener(line) {
            rows[i].opener = true;
            stack.push(i);
        }
    }
    rows
}

/// Absolute line indices currently visible, honouring collapsed openers.
///
/// A collapsed opener is shown but its interior *and* matching closer are
/// hidden (the closer is represented by a `⋯}` marker on the opener line).
pub fn visible_lines(rows: &[RowMeta], collapsed: &[bool]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        out.push(i);
        if rows[i].opener && collapsed[i] {
            i = rows[i].close_idx + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// The opener whose fold should toggle when the cursor is on `line`: the
/// line itself if it is an opener, otherwise the innermost enclosing opener.
pub fn opener_at(rows: &[RowMeta], line: usize) -> Option<usize> {
    if rows.get(line).is_some_and(|r| r.opener) {
        return Some(line);
    }
    let mut best = None;
    for (o, r) in rows.iter().enumerate() {
        if r.opener && o < line && line <= r.close_idx {
            best = Some(o); // later opener == innermost enclosing
        }
    }
    best
}

/// The innermost opener strictly enclosing `line`, or `None` at top level.
///
/// Unlike [`opener_at`], an opener line resolves to its *parent*, never
/// itself — this is the node `←` steps out to. A closing-bracket line counts
/// as inside its own block, so it resolves to that block's opener.
pub fn parent_of(rows: &[RowMeta], line: usize) -> Option<usize> {
    let mut best = None;
    for (o, r) in rows.iter().enumerate() {
        if o >= line {
            break;
        }
        if r.opener && line <= r.close_idx {
            best = Some(o); // later opener == innermost enclosing
        }
    }
    best
}

/// Direct children of the block opened at `opener`: each nested block is
/// represented by its opener line only, its interior and closer skipped.
fn children(rows: &[RowMeta], opener: usize) -> impl Iterator<Item = usize> + '_ {
    let close = rows[opener].close_idx;
    let mut i = opener + 1;
    std::iter::from_fn(move || {
        if i >= close {
            return None;
        }
        let cur = i;
        i = if rows[cur].opener {
            rows[cur].close_idx + 1
        } else {
            cur + 1
        };
        Some(cur)
    })
}

/// First direct child line of the block at `opener`, if it has any.
pub fn first_child(rows: &[RowMeta], opener: usize) -> Option<usize> {
    children(rows, opener).next()
}

/// Last direct child line of the block at `opener`. A nested block child
/// is reported as its opener line, not its closing bracket.
pub fn last_child(rows: &[RowMeta], opener: usize) -> Option<usize> {
    children(rows, opener).last()
}

/// Expand every collapsed opener that encloses `target`, so a jump to a
/// hidden line (e.g. a search hit) reveals it.
pub fn expand_ancestors(rows: &[RowMeta], collapsed: &mut [bool], target: usize) {
    for (o, r) in rows.iter().enumerate() {
        if r.opener && o < target && target <= r.close_idx {
            collapsed[o] = false;
        }
    }
}

/// Case-insensitive substring match over every line, returning the absolute
/// indices that contain `query`. Empty query matches nothing.
pub fn find_matches(lines: &[String], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return Vec::new();
    }
    let needle = query.to_lowercase();
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.to_lowercase().contains(&needle))
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(str::to_owned).collect()
    }

    const SAMPLE: &str = r#"{
  "schemaVersion": 2,
  "config": {
    "digest": "sha256:abc",
    "size": 1234
  },
  "layers": [
    {
      "digest": "sha256:def"
    }
  ]
}"#;

    #[test]
    fn openers_match_their_closers() {
        let ls = lines(SAMPLE);
        let rows = build_rows(&ls);
        // line 0 `{` closes at last line `}`
        assert!(rows[0].opener);
        assert_eq!(rows[0].close_idx, ls.len() - 1);
        // `"config": {` (line 2) closes at `},` (line 5)
        assert!(rows[2].opener);
        assert_eq!(rows[2].close_idx, 5);
        // `"layers": [` (line 6) closes at `]` (line 10)
        assert!(rows[6].opener);
        assert_eq!(rows[6].close_idx, 10);
        // nested object inside the array (line 7) closes at line 9
        assert!(rows[7].opener);
        assert_eq!(rows[7].close_idx, 9);
        // a leaf is its own close index and not an opener
        assert!(!rows[1].opener);
        assert_eq!(rows[1].close_idx, 1);
    }

    #[test]
    fn inline_empty_containers_are_leaves() {
        let ls = lines("{\n  \"a\": {},\n  \"b\": []\n}");
        let rows = build_rows(&ls);
        assert!(!rows[1].opener); // "a": {}
        assert!(!rows[2].opener); // "b": []
    }

    #[test]
    fn collapsing_hides_exactly_the_interior_and_closer() {
        let ls = lines(SAMPLE);
        let rows = build_rows(&ls);
        let mut collapsed = vec![false; ls.len()];
        collapsed[2] = true; // collapse "config"
        let vis = visible_lines(&rows, &collapsed);
        // lines 3,4,5 (interior + closer) hidden; everything else shown
        assert!(!vis.contains(&3));
        assert!(!vis.contains(&4));
        assert!(!vis.contains(&5));
        assert!(vis.contains(&2)); // opener still visible
        assert!(vis.contains(&6)); // sibling still visible
        assert_eq!(vis.len(), ls.len() - 3);
    }

    #[test]
    fn collapsing_root_hides_all_but_opener() {
        let ls = lines(SAMPLE);
        let rows = build_rows(&ls);
        let mut collapsed = vec![false; ls.len()];
        collapsed[0] = true;
        let vis = visible_lines(&rows, &collapsed);
        assert_eq!(vis, vec![0]);
    }

    #[test]
    fn nested_collapse_survives_parent_expand() {
        let ls = lines(SAMPLE);
        let rows = build_rows(&ls);
        let mut collapsed = vec![false; ls.len()];
        collapsed[7] = true; // collapse the object inside layers
        collapsed[6] = true; // collapse layers itself
        collapsed[6] = false; // re-expand layers
        let vis = visible_lines(&rows, &collapsed);
        // layers interior visible again, but the nested object stays folded
        assert!(vis.contains(&7));
        assert!(!vis.contains(&8)); // "digest" inside the folded object
    }

    #[test]
    fn opener_at_resolves_enclosing_opener() {
        let ls = lines(SAMPLE);
        let rows = build_rows(&ls);
        assert_eq!(opener_at(&rows, 2), Some(2)); // on an opener
        assert_eq!(opener_at(&rows, 3), Some(2)); // inside config
        assert_eq!(opener_at(&rows, 8), Some(7)); // innermost, not layers/root
        assert_eq!(opener_at(&rows, 1), Some(0)); // top-level leaf → root
    }

    #[test]
    fn parent_of_resolves_the_enclosing_block_never_self() {
        let ls = lines(SAMPLE);
        let rows = build_rows(&ls);
        assert_eq!(parent_of(&rows, 0), None); // document root
        assert_eq!(parent_of(&rows, 1), Some(0)); // top-level leaf
        assert_eq!(parent_of(&rows, 2), Some(0)); // opener → its parent, not itself
        assert_eq!(parent_of(&rows, 3), Some(2)); // leaf in config
        assert_eq!(parent_of(&rows, 5), Some(2)); // config's closer
        assert_eq!(parent_of(&rows, 7), Some(6)); // object in layers
        assert_eq!(parent_of(&rows, 8), Some(7)); // innermost, not layers
        assert_eq!(parent_of(&rows, 10), Some(6)); // closer sits inside its own block
    }

    #[test]
    fn first_and_last_child_skip_nested_interiors() {
        let ls = lines(SAMPLE);
        let rows = build_rows(&ls);
        assert_eq!(first_child(&rows, 0), Some(1));
        assert_eq!(last_child(&rows, 0), Some(6)); // layers opener, not its `]`
        assert_eq!(first_child(&rows, 2), Some(3));
        assert_eq!(last_child(&rows, 2), Some(4));
        assert_eq!(first_child(&rows, 6), Some(7));
        assert_eq!(last_child(&rows, 6), Some(7));
        assert_eq!(first_child(&rows, 7), Some(8));
    }

    #[test]
    fn empty_block_has_no_children() {
        let rows = build_rows(&lines("{\n}"));
        assert_eq!(first_child(&rows, 0), None);
        assert_eq!(last_child(&rows, 0), None);
    }

    #[test]
    fn expand_ancestors_reveals_a_hidden_target() {
        let ls = lines(SAMPLE);
        let rows = build_rows(&ls);
        let mut collapsed = vec![false; ls.len()];
        collapsed[0] = true;
        collapsed[6] = true;
        collapsed[7] = true;
        expand_ancestors(&rows, &mut collapsed, 8); // reveal "digest" at line 8
        let vis = visible_lines(&rows, &collapsed);
        assert!(vis.contains(&8));
    }

    #[test]
    fn find_matches_is_case_insensitive() {
        let ls = lines(SAMPLE);
        assert_eq!(find_matches(&ls, "DIGEST"), vec![3, 8]);
        assert!(find_matches(&ls, "").is_empty());
        assert!(find_matches(&ls, "nope").is_empty());
    }

    #[test]
    fn close_bracket_maps_openers() {
        assert_eq!(close_bracket("  \"a\": {"), Some('}'));
        assert_eq!(close_bracket("  \"a\": ["), Some(']'));
        assert_eq!(close_bracket("  \"a\": 1,"), None);
    }

    #[test]
    fn malformed_json_has_no_openers() {
        // pretty_json fell back to lossy plain text (no braces)
        let ls = lines("not json\njust text\nlines");
        let rows = build_rows(&ls);
        assert!(rows.iter().all(|r| !r.opener));
        let collapsed = vec![false; ls.len()];
        assert_eq!(visible_lines(&rows, &collapsed).len(), ls.len());
    }
}

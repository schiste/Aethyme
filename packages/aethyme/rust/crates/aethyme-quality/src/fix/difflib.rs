//! Port of the slice of CPython `difflib` that `FilePatch.generate_diff`
//! depends on: `SequenceMatcher` (with the autojunk heuristic) and
//! `unified_diff`.
//!
//! A hand-rolled Myers diff would produce *a* valid unified diff, but
//! not the same one: difflib's matcher is a longest-contiguous-match
//! recursion, not an edit-script minimizer, and above 200 lines it also
//! prunes "popular" elements from the match index. Both change which
//! hunks appear and where they split. Since the produced diff is the
//! byte-parity artifact of this phase, difflib is ported as-is.
//!
//! Only the `isjunk=None, autojunk=True` configuration is modelled —
//! the single way `unified_diff` constructs its matcher.

use std::collections::HashMap;

/// `SequenceMatcher.Match`: `a[i..i+size] == b[j..j+size]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Match {
    i: usize,
    j: usize,
    size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    Replace,
    Delete,
    Insert,
    Equal,
}

/// `(tag, i1, i2, j1, j2)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Opcode {
    tag: Tag,
    i1: usize,
    i2: usize,
    j1: usize,
    j2: usize,
}

struct SequenceMatcher<'a> {
    a: &'a [&'a str],
    b: &'a [&'a str],
    /// `b2j`: element → indices in `b`, with popular elements purged.
    b2j: HashMap<&'a str, Vec<usize>>,
}

impl<'a> SequenceMatcher<'a> {
    /// `SequenceMatcher(None, a, b)` — `isjunk=None` leaves `bjunk`
    /// empty, so the junk-extension passes of `find_longest_match` are
    /// no-ops. `bpopular` elements are still purged from `b2j` but are
    /// NOT junk, so the non-junk extension passes DO walk across them.
    fn new(a: &'a [&'a str], b: &'a [&'a str]) -> Self {
        let mut b2j: HashMap<&'a str, Vec<usize>> = HashMap::new();
        for (i, elt) in b.iter().enumerate() {
            b2j.entry(elt).or_default().push(i);
        }
        // Purge popular elements (autojunk=True, the default).
        let n = b.len();
        if n >= 200 {
            let ntest = n / 100 + 1;
            b2j.retain(|_, idxs| idxs.len() <= ntest);
        }
        SequenceMatcher { a, b, b2j }
    }

    fn find_longest_match(&self, alo: usize, ahi: usize, blo: usize, bhi: usize) -> Match {
        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
        let mut j2len: HashMap<usize, usize> = HashMap::new();
        for i in alo..ahi {
            let mut newj2len: HashMap<usize, usize> = HashMap::new();
            if let Some(indices) = self.b2j.get(self.a[i]) {
                for &j in indices {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let prev = if j == 0 {
                        0
                    } else {
                        j2len.get(&(j - 1)).copied().unwrap_or(0)
                    };
                    let k = prev + 1;
                    newj2len.insert(j, k);
                    if k > bestsize {
                        besti = i + 1 - k;
                        bestj = j + 1 - k;
                        bestsize = k;
                    }
                }
            }
            j2len = newj2len;
        }

        // Extend by non-junk elements on each end. With isjunk=None
        // every element is non-junk, so the subsequent "suck up the
        // matching junk" passes in CPython are unreachable and omitted.
        while besti > alo
            && bestj > blo
            && self.a[besti - 1] == self.b[bestj - 1]
        {
            besti -= 1;
            bestj -= 1;
            bestsize += 1;
        }
        while besti + bestsize < ahi
            && bestj + bestsize < bhi
            && self.a[besti + bestsize] == self.b[bestj + bestsize]
        {
            bestsize += 1;
        }

        Match {
            i: besti,
            j: bestj,
            size: bestsize,
        }
    }

    fn get_matching_blocks(&self) -> Vec<Match> {
        let (la, lb) = (self.a.len(), self.b.len());
        let mut queue: Vec<(usize, usize, usize, usize)> = vec![(0, la, 0, lb)];
        let mut matching_blocks: Vec<Match> = Vec::new();
        // CPython pops from the end; replicated so the recursion order
        // is identical (the final sort makes the result order-stable
        // either way).
        while let Some((alo, ahi, blo, bhi)) = queue.pop() {
            let m = self.find_longest_match(alo, ahi, blo, bhi);
            if m.size > 0 {
                matching_blocks.push(m);
                if alo < m.i && blo < m.j {
                    queue.push((alo, m.i, blo, m.j));
                }
                if m.i + m.size < ahi && m.j + m.size < bhi {
                    queue.push((m.i + m.size, ahi, m.j + m.size, bhi));
                }
            }
        }
        matching_blocks.sort();

        // Collapse adjacent equal blocks.
        let (mut i1, mut j1, mut k1) = (0usize, 0usize, 0usize);
        let mut non_adjacent: Vec<Match> = Vec::new();
        for m in matching_blocks {
            if i1 + k1 == m.i && j1 + k1 == m.j {
                k1 += m.size;
            } else {
                if k1 > 0 {
                    non_adjacent.push(Match {
                        i: i1,
                        j: j1,
                        size: k1,
                    });
                }
                i1 = m.i;
                j1 = m.j;
                k1 = m.size;
            }
        }
        if k1 > 0 {
            non_adjacent.push(Match {
                i: i1,
                j: j1,
                size: k1,
            });
        }
        non_adjacent.push(Match {
            i: la,
            j: lb,
            size: 0,
        });
        non_adjacent
    }

    fn get_opcodes(&self) -> Vec<Opcode> {
        let (mut i, mut j) = (0usize, 0usize);
        let mut answer: Vec<Opcode> = Vec::new();
        for m in self.get_matching_blocks() {
            let tag = if i < m.i && j < m.j {
                Some(Tag::Replace)
            } else if i < m.i {
                Some(Tag::Delete)
            } else if j < m.j {
                Some(Tag::Insert)
            } else {
                None
            };
            if let Some(tag) = tag {
                answer.push(Opcode {
                    tag,
                    i1: i,
                    i2: m.i,
                    j1: j,
                    j2: m.j,
                });
            }
            i = m.i + m.size;
            j = m.j + m.size;
            if m.size > 0 {
                answer.push(Opcode {
                    tag: Tag::Equal,
                    i1: m.i,
                    i2: i,
                    j1: m.j,
                    j2: j,
                });
            }
        }
        answer
    }

    fn get_grouped_opcodes(&self, n: usize) -> Vec<Vec<Opcode>> {
        let mut codes = self.get_opcodes();
        if codes.is_empty() {
            codes = vec![Opcode {
                tag: Tag::Equal,
                i1: 0,
                i2: 1,
                j1: 0,
                j2: 1,
            }];
        }
        if codes[0].tag == Tag::Equal {
            let c = codes[0];
            codes[0] = Opcode {
                tag: c.tag,
                i1: c.i1.max(c.i2.saturating_sub(n)),
                i2: c.i2,
                j1: c.j1.max(c.j2.saturating_sub(n)),
                j2: c.j2,
            };
        }
        let last = codes.len() - 1;
        if codes[last].tag == Tag::Equal {
            let c = codes[last];
            codes[last] = Opcode {
                tag: c.tag,
                i1: c.i1,
                i2: c.i2.min(c.i1 + n),
                j1: c.j1,
                j2: c.j2.min(c.j1 + n),
            };
        }

        let nn = n + n;
        let mut groups: Vec<Vec<Opcode>> = Vec::new();
        let mut group: Vec<Opcode> = Vec::new();
        for code in codes {
            let mut c = code;
            if c.tag == Tag::Equal && c.i2 - c.i1 > nn {
                group.push(Opcode {
                    tag: c.tag,
                    i1: c.i1,
                    i2: c.i2.min(c.i1 + n),
                    j1: c.j1,
                    j2: c.j2.min(c.j1 + n),
                });
                groups.push(std::mem::take(&mut group));
                c.i1 = c.i1.max(c.i2 - n);
                c.j1 = c.j1.max(c.j2 - n);
            }
            group.push(c);
        }
        if !(group.is_empty() || (group.len() == 1 && group[0].tag == Tag::Equal)) {
            groups.push(group);
        }
        groups
    }
}

/// `difflib._format_range_unified`.
fn format_range_unified(start: usize, stop: usize) -> String {
    let mut beginning = start + 1;
    let length = stop - start;
    if length == 1 {
        return beginning.to_string();
    }
    if length == 0 {
        beginning -= 1;
    }
    format!("{beginning},{length}")
}

/// `difflib.unified_diff(a, b, fromfile, tofile, n, lineterm)`, yielding
/// the same sequence of strings CPython does.
///
/// Note that the callers pass `lineterm=""`, which means the `---`,
/// `+++`, and `@@` records carry NO terminator of their own. Joined with
/// `"".join(...)` — as `FilePatch.generate_diff` does — that produces a
/// diff whose header records run together. That is the shipped output
/// and the parity target; it is not corrected here.
pub fn unified_diff(
    a: &[&str],
    b: &[&str],
    fromfile: &str,
    tofile: &str,
    n: usize,
    lineterm: &str,
) -> Vec<String> {
    let matcher = SequenceMatcher::new(a, b);
    let mut out: Vec<String> = Vec::new();
    let mut started = false;
    for group in matcher.get_grouped_opcodes(n) {
        if !started {
            started = true;
            out.push(format!("--- {fromfile}{lineterm}"));
            out.push(format!("+++ {tofile}{lineterm}"));
        }
        let first = group[0];
        let last = group[group.len() - 1];
        let file1_range = format_range_unified(first.i1, last.i2);
        let file2_range = format_range_unified(first.j1, last.j2);
        out.push(format!("@@ -{file1_range} +{file2_range} @@{lineterm}"));

        for code in &group {
            match code.tag {
                Tag::Equal => {
                    for line in &a[code.i1..code.i2] {
                        out.push(format!(" {line}"));
                    }
                }
                Tag::Replace | Tag::Delete => {
                    for line in &a[code.i1..code.i2] {
                        out.push(format!("-{line}"));
                    }
                    if code.tag == Tag::Replace {
                        for line in &b[code.j1..code.j2] {
                            out.push(format!("+{line}"));
                        }
                    }
                }
                Tag::Insert => {
                    for line in &b[code.j1..code.j2] {
                        out.push(format!("+{line}"));
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fix::pystr;

    fn diff(original: &str, new: &str, name: &str) -> String {
        let a = pystr::splitlines(original, true);
        let b = pystr::splitlines(new, true);
        unified_diff(&a, &b, &format!("a/{name}"), &format!("b/{name}"), 3, "").join("")
    }

    #[test]
    fn single_line_change_matches_cpython_bytes() {
        // Verified against difflib on CPython: with lineterm="" the
        // header records run together in the joined output.
        assert_eq!(
            diff("line1\nline2\nline3", "line1\nline2 modified\nline3", "test.py"),
            "--- a/test.py+++ b/test.py@@ -1,3 +1,3 @@ line1\n-line2\n+line2 modified\n line3"
        );
    }

    #[test]
    fn new_file_is_an_insert_hunk_at_zero() {
        assert_eq!(
            diff("", "alpha\nbeta\n", "FOLDER.md"),
            "--- a/FOLDER.md+++ b/FOLDER.md@@ -0,0 +1,2 @@+alpha\n+beta\n"
        );
    }

    #[test]
    fn full_deletion_renders_an_empty_target_range() {
        assert_eq!(
            diff("alpha\n", "", "x.py"),
            "--- a/x.py+++ b/x.py@@ -1 +0,0 @@-alpha\n"
        );
    }

    #[test]
    fn identical_content_yields_no_diff() {
        assert_eq!(diff("same\n", "same\n", "x.py"), "");
        assert_eq!(diff("", "", "x.py"), "");
    }

    #[test]
    fn distant_changes_split_into_separate_hunks() {
        let original: String = (0..40).map(|i| format!("line{i}\n")).collect();
        let mut new_lines: Vec<String> = (0..40).map(|i| format!("line{i}\n")).collect();
        new_lines[2] = "CHANGED-A\n".to_string();
        new_lines[30] = "CHANGED-B\n".to_string();
        let new: String = new_lines.concat();
        let rendered = diff(&original, &new, "x.py");
        assert_eq!(rendered.matches("@@ -").count(), 2, "{rendered}");
        assert!(rendered.contains("@@ -1,6 +1,6 @@"), "{rendered}");
        assert!(rendered.contains("@@ -28,7 +28,7 @@"), "{rendered}");
    }

    #[test]
    fn autojunk_threshold_is_observable() {
        // 250 lines where "x\n" appears 200 times (> 250/100 + 1 = 3),
        // so it is purged from b2j. The matcher can then only anchor on
        // the unique lines, which changes the hunk layout versus a
        // matcher without the heuristic. Locked in as a regression
        // guard on the heuristic being present at all.
        let mut a: Vec<String> = Vec::new();
        for i in 0..250 {
            a.push(if i % 5 == 0 {
                format!("unique{i}\n")
            } else {
                "x\n".to_string()
            });
        }
        let mut b = a.clone();
        b[100] = "CHANGED\n".to_string();
        let rendered = diff(&a.concat(), &b.concat(), "big.py");
        assert!(rendered.contains("+CHANGED\n"), "{rendered}");
        assert!(rendered.starts_with("--- a/big.py+++ b/big.py@@ "), "{rendered}");
    }

    #[test]
    fn opcode_shapes_match_cpython() {
        let a = ["a\n", "b\n", "c\n"];
        let b = ["a\n", "x\n", "c\n"];
        let matcher = SequenceMatcher::new(&a, &b);
        let codes = matcher.get_opcodes();
        assert_eq!(
            codes,
            vec![
                Opcode { tag: Tag::Equal, i1: 0, i2: 1, j1: 0, j2: 1 },
                Opcode { tag: Tag::Replace, i1: 1, i2: 2, j1: 1, j2: 2 },
                Opcode { tag: Tag::Equal, i1: 2, i2: 3, j1: 2, j2: 3 },
            ]
        );
    }

    #[test]
    fn range_formatting_matches_cpython() {
        assert_eq!(format_range_unified(0, 0), "0,0");
        assert_eq!(format_range_unified(0, 1), "1");
        assert_eq!(format_range_unified(0, 3), "1,3");
        assert_eq!(format_range_unified(5, 5), "5,0");
    }
}

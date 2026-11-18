use super::target::Target;

const JOIN_GAP: u32 = 2;
const SPLIT_AFTER: u32 = 20;
const MAX_HUNK: u32 = 80;
const WINDOW: u32 = 10;
const MAX_CONTEXT: u32 = 150;
const TRIMMED_WINDOW: u32 = 40;

#[derive(Debug, Clone, PartialEq)]
pub struct Hunk {
    pub start: u32,
    pub end: u32,
    pub context_start: u32,
    pub context_end: u32,
}

fn indent(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    (!trimmed.is_empty()).then(|| line.len() - trimmed.len())
}

fn starts_item(line: &str) -> bool {
    indent(line) == Some(0) && !line.starts_with(['}', ')', ']'])
}

fn ranges(target: &Target) -> Vec<(u32, u32)> {
    let total = target.lines.len() as u32;
    let Some(changed) = &target.changed else {
        return if total == 0 {
            Vec::new()
        } else {
            vec![(1, total)]
        };
    };
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &line in changed.iter().filter(|line| **line >= 1 && **line <= total) {
        match out.last_mut() {
            Some((_, end)) if line <= *end + 1 + JOIN_GAP => *end = line,
            _ => out.push((line, line)),
        }
    }
    out
}

fn split(lines: &[String], start: u32, end: u32) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut from = start;
    for line in start + 1..=end {
        let length = line - from;
        let boundary = length >= SPLIT_AFTER && starts_item(&lines[line as usize - 1]);
        if boundary || length >= MAX_HUNK {
            out.push((from, line - 1));
            from = line;
        }
    }
    out.push((from, end));
    out
}

fn context(lines: &[String], start: u32, end: u32) -> (u32, u32) {
    let total = lines.len() as u32;
    let text = |line: u32| lines[line as usize - 1].as_str();
    let window = |size: u32| (start.saturating_sub(size).max(1), (end + size).min(total));
    let nested = (start..=end)
        .filter_map(|line| indent(text(line)))
        .min()
        .is_some_and(|least| least > 0);
    if !nested {
        return window(WINDOW);
    }
    let top = (1..start)
        .rev()
        .find(|line| indent(text(*line)) == Some(0))
        .unwrap_or(1);
    let below = (end + 1..=total).find(|line| indent(text(*line)) == Some(0));
    let bottom = match below {
        Some(line) if starts_item(text(line)) => line - 1,
        Some(line) => line,
        None => total,
    };
    if bottom - top > MAX_CONTEXT {
        return window(TRIMMED_WINDOW);
    }
    (top, bottom)
}

pub fn hunks(target: &Target) -> Vec<Hunk> {
    let pieces = ranges(target)
        .into_iter()
        .flat_map(|(start, end)| split(&target.lines, start, end));
    let with_code = pieces.filter(|(start, end)| {
        (*start..=*end).any(|line| indent(&target.lines[line as usize - 1]).is_some())
    });
    let hunks = with_code.map(|(start, end)| {
        let (context_start, context_end) = context(&target.lines, start, end);
        Hunk {
            start,
            end,
            context_start,
            context_end,
        }
    });
    hunks.collect()
}

impl Hunk {
    pub fn code(&self, target: &Target) -> String {
        let lines = (self.context_start..=self.context_end)
            .map(|line| format!("{line:>5}| {}\n", target.lines[line as usize - 1]));
        lines.collect()
    }

    pub fn span(&self) -> String {
        if self.start == self.end {
            return format!("line {}", self.start);
        }
        format!("lines {}-{}", self.start, self.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str =
        "use a;\n\nfn first() {\n    one();\n    two();\n}\n\nfn second() {\n    three();\n}\n";

    fn target(changed: Option<&[u32]>) -> Target {
        Target {
            path: "a.rs".to_string(),
            lines: SOURCE.lines().map(String::from).collect(),
            changed: changed.map(|lines| lines.iter().copied().collect()),
            diff: String::new(),
        }
    }

    #[test]
    fn a_nested_change_is_shown_with_its_enclosing_item() {
        let found = hunks(&target(Some(&[5])));
        assert_eq!(
            found,
            vec![Hunk {
                start: 5,
                end: 5,
                context_start: 3,
                context_end: 6
            }]
        );
        assert!(found[0]
            .code(&target(None))
            .starts_with("    3| fn first() {"));
    }

    #[test]
    fn nearby_changes_join_and_distant_ones_stay_apart() {
        let found = hunks(&target(Some(&[4, 5, 9])));
        let spans: Vec<(u32, u32)> = found.iter().map(|hunk| (hunk.start, hunk.end)).collect();
        assert_eq!(spans, vec![(4, 5), (9, 9)]);
        assert_eq!((found[1].context_start, found[1].context_end), (8, 10));
    }

    #[test]
    fn a_top_level_change_gets_a_window_and_blank_changes_are_dropped() {
        let found = hunks(&target(Some(&[1, 7])));
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].start, found[0].context_start, found[0].context_end),
            (1, 1, 10)
        );
    }

    #[test]
    fn a_whole_new_file_is_split_at_item_boundaries() {
        let body: String = (0..30)
            .map(|n| format!("fn f{n}() {{\n    work();\n}}\n\n"))
            .collect();
        let target = Target {
            path: "a.rs".to_string(),
            lines: body.lines().map(String::from).collect(),
            changed: None,
            diff: String::new(),
        };
        let found = hunks(&target);
        assert!(found.len() > 3);
        assert!(found.iter().all(|hunk| hunk.end - hunk.start < MAX_HUNK));
        assert!(found
            .iter()
            .skip(1)
            .all(|hunk| target.lines[hunk.start as usize - 1].starts_with("fn ")));
    }
}

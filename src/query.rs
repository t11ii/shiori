//! Small search language used by the search box and smart folders.
//!
//! ```text
//! 旅行 #家族 -#ボツ star:3 label:red ext:jpg,png is:untagged is:dir is:comment
//! ```
//! * plain words match the file name or comment (all words must match)
//! * `#tag` / `tag:tag` requires a tag, `-#tag` / `-tag:tag` excludes it
//! * `star:N` (or `★N`) = at least N stars
//! * `label:red` / `label:赤` (comma separated for OR)
//! * `ext:jpg,png`
//! * `is:untagged`, `is:tagged`, `is:comment`, `is:dir`, `is:file`, `is:missing`
//! * `"quoted words"` are matched literally

use crate::db::LABELS;
use rusqlite::types::Value;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filter {
    pub words: Vec<String>,
    pub not_words: Vec<String>,
    pub tags: Vec<String>,
    pub not_tags: Vec<String>,
    pub min_star: Option<u8>,
    pub labels: Vec<u8>,
    pub exts: Vec<String>,
    pub untagged: bool,
    pub tagged: bool,
    pub has_comment: bool,
    pub only_dirs: bool,
    pub only_files: bool,
    pub missing: bool,
}

fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn label_from_name(s: &str) -> Option<u8> {
    let s = s.to_lowercase();
    LABELS
        .iter()
        .position(|(en, ja, _)| *en == s || *ja == s)
        .map(|i| i as u8)
        .or_else(|| {
            s.parse::<u8>()
                .ok()
                .filter(|n| (*n as usize) < LABELS.len())
        })
}

impl Filter {
    pub fn parse(s: &str) -> Self {
        let mut f = Filter::default();
        for tok in tokenize(s) {
            let (neg, t) = match tok.strip_prefix('-') {
                Some(rest) if !rest.is_empty() => (true, rest.to_string()),
                _ => (false, tok.clone()),
            };
            let tag = t
                .strip_prefix('#')
                .or_else(|| t.strip_prefix("tag:"))
                .or_else(|| t.strip_prefix("タグ:"));
            if let Some(tag) = tag.filter(|x| !x.is_empty()) {
                if neg {
                    f.not_tags.push(tag.to_string());
                } else {
                    f.tags.push(tag.to_string());
                }
                continue;
            }
            if let Some(n) = t.strip_prefix("star:").or_else(|| t.strip_prefix('★')) {
                if let Ok(n) = n.parse::<u8>() {
                    f.min_star = Some(n.min(5));
                    continue;
                }
            }
            if let Some(l) = t
                .strip_prefix("label:")
                .or_else(|| t.strip_prefix("ラベル:"))
            {
                let ls: Vec<u8> = l.split(',').filter_map(label_from_name).collect();
                if !ls.is_empty() {
                    f.labels.extend(ls);
                    continue;
                }
            }
            if let Some(e) = t.strip_prefix("ext:") {
                f.exts.extend(
                    e.split(',')
                        .map(|x| x.trim_start_matches('.').to_lowercase())
                        .filter(|x| !x.is_empty()),
                );
                continue;
            }
            if let Some(is) = t.strip_prefix("is:") {
                let known = match is {
                    "untagged" | "notag" => {
                        f.untagged = true;
                        true
                    }
                    "tagged" => {
                        f.tagged = true;
                        true
                    }
                    "comment" | "commented" => {
                        f.has_comment = true;
                        true
                    }
                    "dir" | "folder" => {
                        f.only_dirs = true;
                        true
                    }
                    "file" => {
                        f.only_files = true;
                        true
                    }
                    "missing" => {
                        f.missing = true;
                        true
                    }
                    _ => false,
                };
                if known {
                    continue;
                }
            }
            if neg {
                f.not_words.push(t);
            } else {
                f.words.push(tok);
            }
        }
        f
    }

    pub fn is_empty(&self) -> bool {
        *self == Filter::default()
    }

    /// Combines two filters (logical AND).
    pub fn merged(&self, other: &Filter) -> Filter {
        let mut f = self.clone();
        f.words.extend(other.words.iter().cloned());
        f.not_words.extend(other.not_words.iter().cloned());
        f.tags.extend(other.tags.iter().cloned());
        f.not_tags.extend(other.not_tags.iter().cloned());
        f.min_star = match (f.min_star, other.min_star) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        f.labels.extend(other.labels.iter().cloned());
        f.exts.extend(other.exts.iter().cloned());
        f.untagged |= other.untagged;
        f.tagged |= other.tagged;
        f.has_comment |= other.has_comment;
        f.only_dirs |= other.only_dirs;
        f.only_files |= other.only_files;
        f.missing |= other.missing;
        f
    }

    pub fn to_sql(&self, conds: &mut Vec<String>, args: &mut Vec<Value>) {
        for w in &self.words {
            conds.push("(f.name LIKE ? ESCAPE '\\' OR f.comment LIKE ? ESCAPE '\\')".into());
            let p = like(w);
            args.push(p.clone().into());
            args.push(p.into());
        }
        for w in &self.not_words {
            conds.push("NOT (f.name LIKE ? ESCAPE '\\' OR f.comment LIKE ? ESCAPE '\\')".into());
            let p = like(w);
            args.push(p.clone().into());
            args.push(p.into());
        }
        const HAS_TAG: &str = "EXISTS (SELECT 1 FROM file_tags ft JOIN tags t ON t.id = ft.tag_id WHERE ft.file_id = f.id AND t.name = ?)";
        for t in &self.tags {
            conds.push(HAS_TAG.into());
            args.push(t.clone().into());
        }
        for t in &self.not_tags {
            conds.push(format!("NOT {HAS_TAG}"));
            args.push(t.clone().into());
        }
        if let Some(s) = self.min_star {
            conds.push("f.star >= ?".into());
            args.push((s as i64).into());
        }
        if !self.labels.is_empty() {
            conds.push(format!(
                "f.label IN ({})",
                vec!["?"; self.labels.len()].join(",")
            ));
            args.extend(self.labels.iter().map(|l| Value::from(*l as i64)));
        }
        if !self.exts.is_empty() {
            conds.push(format!(
                "f.ext IN ({})",
                vec!["?"; self.exts.len()].join(",")
            ));
            args.extend(self.exts.iter().map(|e| Value::from(e.clone())));
        }
        if self.untagged {
            conds.push("NOT EXISTS (SELECT 1 FROM file_tags ft WHERE ft.file_id = f.id)".into());
        }
        if self.tagged {
            conds.push("EXISTS (SELECT 1 FROM file_tags ft WHERE ft.file_id = f.id)".into());
        }
        if self.has_comment {
            conds.push("f.comment != ''".into());
        }
        if self.only_dirs {
            conds.push("f.is_dir = 1".into());
        }
        if self.only_files {
            conds.push("f.is_dir = 0".into());
        }
        if self.missing {
            conds.push("f.missing = 1".into());
        }
    }
}

fn like(s: &str) -> String {
    let mut out = String::from("%");
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_tokens() {
        let f = Filter::parse(
            r#"旅行 #家族 -#ボツ star:3 label:red,青 ext:.JPG,png is:untagged "a b" -draft"#,
        );
        assert_eq!(f.words, vec!["旅行", "a b"]);
        assert_eq!(f.not_words, vec!["draft"]);
        assert_eq!(f.tags, vec!["家族"]);
        assert_eq!(f.not_tags, vec!["ボツ"]);
        assert_eq!(f.min_star, Some(3));
        assert_eq!(f.labels, vec![1, 5]);
        assert_eq!(f.exts, vec!["jpg", "png"]);
        assert!(f.untagged);
    }

    #[test]
    fn unknown_prefix_is_a_word() {
        let f = Filter::parse("is:weird star:x");
        assert_eq!(f.words, vec!["is:weird", "star:x"]);
    }

    #[test]
    fn merge_takes_stricter_star() {
        let a = Filter::parse("star:2 #a");
        let b = Filter::parse("star:4 #b");
        let m = a.merged(&b);
        assert_eq!(m.min_star, Some(4));
        assert_eq!(m.tags, vec!["a", "b"]);
    }
}

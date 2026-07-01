//! Production-LOC ceiling per Rust source file.
//!
//! Test code does not count: `#[cfg(test)]`-gated items (both `mod tests {}`
//! blocks and `mod tests;` declarations), files named `tests.rs`, and
//! anything under a `tests/` directory are excluded. Brace matching is
//! string- and comment-aware so `{`/`}` inside literals cannot skew the
//! count.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::{display, read_to_string};

const MAX_RUST_FILE_LINES: usize = 500;

pub(crate) fn check_loc(root: &Path) -> Result<(), String> {
    let rust_files = collect_rust_files(root);
    let mut errors = Vec::new();

    for file in rust_files {
        let content = read_to_string(&file)?;
        let lines = production_line_count(&content);
        if lines > MAX_RUST_FILE_LINES {
            errors.push(format!(
                "{} has {lines} production lines; max is {MAX_RUST_FILE_LINES}",
                display(&file)
            ));
        }
    }

    if errors.is_empty() {
        println!("xtask loc: passed");
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

fn collect_rust_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_rust_files_inner(root, &mut files);
    files
}

fn collect_rust_files_inner(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(OsStr::to_str).unwrap_or("");

        if path.is_dir() {
            if matches!(
                name,
                ".git" | ".codegraph" | "target" | "node_modules" | "dist" | "dist-test" | "tests"
            ) {
                continue;
            }
            collect_rust_files_inner(&path, files);
        } else if path.extension().and_then(OsStr::to_str) == Some("rs") && name != "tests.rs" {
            files.push(path);
        }
    }
}

fn production_line_count(content: &str) -> usize {
    let lines: Vec<&str> = content.lines().collect();
    let mut count = 0;
    let mut index = 0;

    while index < lines.len() {
        if starts_cfg_test_item(&lines, index) {
            index = skip_cfg_test_item(&lines, index);
        } else {
            count += 1;
            index += 1;
        }
    }

    count
}

fn starts_cfg_test_item(lines: &[&str], index: usize) -> bool {
    lines[index].trim() == "#[cfg(test)]"
}

/// Skips the `#[cfg(test)]` attribute and the item it gates: either a
/// semicolon declaration (`mod tests;`) or a braced item.
fn skip_cfg_test_item(lines: &[&str], start: usize) -> usize {
    let index = start + 1;
    let Some(first) = lines.get(index) else {
        return index;
    };
    if first.trim_end().ends_with(';') {
        return index + 1;
    }
    skip_braced_item(lines, index)
}

/// Advances past a braced item, ignoring braces inside string literals,
/// character literals, and comments.
fn skip_braced_item(lines: &[&str], start: usize) -> usize {
    let mut depth = 0i32;
    let mut saw_open = false;
    let mut in_block_comment = false;

    for (offset, line) in lines[start..].iter().enumerate() {
        scan_line(line, &mut depth, &mut saw_open, &mut in_block_comment);
        if saw_open && depth <= 0 {
            return start + offset + 1;
        }
    }

    lines.len()
}

fn scan_line(line: &str, depth: &mut i32, saw_open: &mut bool, in_block_comment: &mut bool) {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    let mut in_char = false;
    let mut in_raw_string = false;
    let mut raw_hashes = 0usize;

    while i < bytes.len() {
        let c = bytes[i] as char;

        if *in_block_comment {
            if c == '*' && bytes.get(i + 1) == Some(&b'/') {
                *in_block_comment = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if in_raw_string {
            if c == '"' && line[i + 1..].starts_with(&"#".repeat(raw_hashes)) {
                in_raw_string = false;
                i += 1 + raw_hashes;
                continue;
            }
            i += 1;
            continue;
        }
        if in_string {
            match c {
                '\\' => i += 2,
                '"' => {
                    in_string = false;
                    i += 1;
                }
                _ => i += 1,
            }
            continue;
        }
        if in_char {
            match c {
                '\\' => i += 2,
                '\'' => {
                    in_char = false;
                    i += 1;
                }
                _ => i += 1,
            }
            continue;
        }

        match c {
            '/' if bytes.get(i + 1) == Some(&b'/') => return, // line comment
            '/' if bytes.get(i + 1) == Some(&b'*') => {
                *in_block_comment = true;
                i += 2;
            }
            'r' if bytes.get(i + 1) == Some(&b'"') || bytes.get(i + 1) == Some(&b'#') => {
                let mut hashes = 0;
                let mut j = i + 1;
                while bytes.get(j) == Some(&b'#') {
                    hashes += 1;
                    j += 1;
                }
                if bytes.get(j) == Some(&b'"') {
                    in_raw_string = true;
                    raw_hashes = hashes;
                    i = j + 1;
                } else {
                    i += 1;
                }
            }
            '"' => {
                in_string = true;
                i += 1;
            }
            '\'' => {
                // Lifetimes ('a) vs char literals ('a'): treat as a char
                // literal only when a closing quote appears nearby.
                let looks_like_char = line[i + 1..]
                    .char_indices()
                    .take(4)
                    .any(|(_, next)| next == '\'');
                if looks_like_char {
                    in_char = true;
                }
                i += 1;
            }
            '{' => {
                *depth += 1;
                *saw_open = true;
                i += 1;
            }
            '}' if *saw_open => {
                *depth -= 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_production_lines_only() {
        let content = r#"fn real() {}

#[cfg(test)]
mod tests {
    #[test]
    fn t() { let brace = "}"; }
}
"#;
        assert_eq!(production_line_count(content), 2);
    }

    #[test]
    fn semicolon_test_module_declarations_do_not_swallow_the_file() {
        let content = "fn a() {}\n#[cfg(test)]\nmod tests;\nfn b() {}\n";

        assert_eq!(production_line_count(content), 2);
    }

    #[test]
    fn braces_in_strings_and_comments_do_not_confuse_the_skip() {
        let content = r##"fn real() {}
#[cfg(test)]
mod tests {
    // a comment with }
    const RAW: &str = r#"{"key": "}"}"#;
    fn t() { let c = '}'; }
}
fn also_real() {}
"##;
        assert_eq!(production_line_count(content), 2);
    }
}

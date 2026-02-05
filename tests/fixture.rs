#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;

pub fn read_fixture(name: &str) -> String {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests");
    path.push("fixtures");
    path.push(name);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("failed to read fixture {name}: {err}"))
}

pub fn read_unescaped(name: &str) -> String {
    let raw = read_fixture(name);
    let mut normalized = raw.replace("\r\n", "\n");
    if normalized.ends_with('\n') {
        normalized.pop();
        if normalized.ends_with('\r') {
            normalized.pop();
        }
    }
    unescape(&normalized)
}

pub fn read_lines_unescaped(name: &str) -> Vec<String> {
    let raw = read_fixture(name);
    let mut normalized = raw.replace("\r\n", "\n");
    if normalized.ends_with('\n') {
        normalized.pop();
        if normalized.ends_with('\r') {
            normalized.pop();
        }
    }
    let unescaped = unescape(&normalized);
    if unescaped.is_empty() {
        return Vec::new();
    }
    unescaped.split('\n').map(|line| line.to_string()).collect()
}

pub fn unescape(input: &str) -> String {
    let mut out = String::new();
    let mut iter = input.chars().peekable();

    while let Some(ch) = iter.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }

        match iter.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some('x') => {
                let hi = iter.next();
                let lo = iter.next();
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    if let (Some(h), Some(l)) = (hi.to_digit(16), lo.to_digit(16)) {
                        let byte = ((h << 4) | l) as u8;
                        out.push(byte as char);
                    } else {
                        out.push('\\');
                        out.push('x');
                        out.push(hi);
                        out.push(lo);
                    }
                } else {
                    out.push('\\');
                    out.push('x');
                    if let Some(hi) = hi {
                        out.push(hi);
                    }
                    if let Some(lo) = lo {
                        out.push(lo);
                    }
                }
            }
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }

    out
}

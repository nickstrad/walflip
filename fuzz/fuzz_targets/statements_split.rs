//! Statement splitting, both modes.
//!
//! Line mode (`parse_statements`): no panic; no statement is empty, padded or multi-line;
//! joined with '\n' they equal the input's non-blank trimmed lines.
//!
//! Script mode (`split_script`, Q5): no panic; every statement is non-empty, ends with `;` and
//! is `sqlite3_complete`; joining the statements with '\n' and splitting again gives the same
//! statements; an error is `incomplete input` or a NUL byte.
#![no_main]

use libfuzzer_sys::fuzz_target;
use walflip::db::{is_complete, parse_statements, split_script};

fuzz_target!(|text: &str| {
    let stmts = parse_statements(text);
    for s in &stmts {
        assert!(!s.is_empty());
        assert_eq!(s.trim(), s.as_str());
        assert!(!s.contains('\n'));
    }
    let want: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(stmts.join("\n"), want.join("\n"));

    match split_script(text) {
        Ok(stmts) => {
            for s in &stmts {
                assert!(s.ends_with(';') && s != ";", "{s:?}");
                assert!(is_complete(s), "not complete: {s:?}");
            }
            let again = split_script(&stmts.join("\n")).expect("re-split of split output failed");
            assert_eq!(again, stmts);
        }
        Err(e) => {
            let e = e.to_string();
            assert!(e.contains("incomplete input") || e.contains("NUL"), "{e}");
        }
    }
});

//! Statement splitting. Invariants: no panic; no statement is empty, padded or multi-line;
//! joined with '\n' they equal the input's non-blank trimmed lines.
#![no_main]

use libfuzzer_sys::fuzz_target;
use walflip::db::parse_statements;

fuzz_target!(|text: &str| {
    let stmts = parse_statements(text);
    for s in &stmts {
        assert!(!s.is_empty());
        assert_eq!(s.trim(), s.as_str());
        assert!(!s.contains('\n'));
    }
    let want: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    assert_eq!(stmts.join("\n"), want.join("\n"));
});

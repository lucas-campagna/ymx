pub const ERROR_TABLE: &str = "\
YMX Diagnostic Codes

Code    Stage     Description
──────  ────────  ──────────────────────────────────────────────────────────────
E001    load      YAML parse error or unsupported YAML feature
E002    compile   Unknown component reference
E003    compile   Missing required argument
E004    load      Duplicate component name in the same namespace
E005    compile   File-scope violation (cross-document _ reference)
E006    compile   Ambiguous shortcut
E007    load      Reserved builtin name (map/reduce/merge)
E008    compile   Max-depth exceeded
E009    options   Entry not found (malformed path / missing file / ambiguous stem)
E010    both      Invalid syntax (call-site, escape, math id, _ymx field, _test block)
E011    compile   Math error / builtin argument type error
E012    compile   Positional arg after named arg
E013    compile   Array/object literal as direct call arg
E015    load      Meta-key reserved name ($_ymx, $$test, ...)
E016    compile   Shell execution error (unknown/disallowed backend, spawn failure)
";

pub fn print_errors() {
    print!("{}", ERROR_TABLE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_table_contains_expected_codes() {
        assert!(ERROR_TABLE.contains("E001"), "missing E001");
        assert!(ERROR_TABLE.contains("E016"), "missing E016");
    }
}

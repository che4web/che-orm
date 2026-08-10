#[test]
fn type_safe_query_rejects_invalid_values() {
    let tests = trybuild::TestCases::new();
    tests.compile_fail("tests/ui/*.rs");
}

//! The Phase 0.3 exit criterion, as a test that can be watched failing.
//!
//! > *Adding a new feature flag with no documentation entry turns `just verify`
//! > red.*
//!
//! The enforcement is a compile error, which is the strongest form available —
//! it cannot be skipped, waived in review or merged behind a green tick. What
//! it cannot do is prove itself, because code that does not compile cannot be
//! in the test suite. That is what this file is for: it compiles the failing
//! cases in a subprocess and asserts the error each one produces.

#[test]
fn an_undocumented_setting_does_not_compile() {
    trybuild::TestCases::new().compile_fail("tests/ui/*.rs");
}

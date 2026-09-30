use trycmd::TestCases;

#[test]
fn sqry_help_snapshot() {
    TestCases::new()
        .default_bin_name("sqry")
        .case("tests/cases/help_root.trycmd")
        .insert_var("[VERSION]", env!("CARGO_PKG_VERSION"))
        .unwrap()
        .env("COLUMNS", "100")
        .env("NO_COLOR", "1");
}

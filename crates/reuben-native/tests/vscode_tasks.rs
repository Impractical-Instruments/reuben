//! The repo's own `.vscode/tasks.json` is the `editors/vscode/tasks.json` template with every task
//! starting `reuben` through `cargo run` instead of the binary the `reuben.command` setting names.
//! Any other difference is drift between the two copies.

use std::path::Path;

const CARGO_RUN: &str = r#""run", "-q", "-p", "reuben-native", "--bin", "reuben", "--", "#;

/// The file with its leading comment block dropped: the two copies introduce themselves
/// differently.
fn body(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    text.lines()
        .skip_while(|line| line.starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn repo_tasks_are_the_template_run_through_cargo() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let template = body(&root.join("editors/vscode/tasks.json"));
    assert_eq!(template.matches("${config:reuben.command}").count(), 3);
    let expected = template
        .replace(r#""${config:reuben.command}""#, r#""cargo""#)
        .replace(
            r#""args": ["live""#,
            &format!(r#""args": [{CARGO_RUN}"live""#),
        )
        .replace(
            r#""args": ["send""#,
            &format!(r#""args": [{CARGO_RUN}"send""#),
        );
    assert_eq!(body(&root.join(".vscode/tasks.json")), expected);
}

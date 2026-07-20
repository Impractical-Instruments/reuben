//! Naming rules shared by every contract consumer.
//!
//! The mapping from a port/param *name* to its `IN_`/`OUT_`/`P_` const fragment, and between an
//! operator's `type_name` and its Rust struct name, lives here so the macro and the scaffold can
//! never disagree on what `freq` becomes (`FREQ`) or what `my_op` becomes (`MyOp`).

/// `freq` -> `FREQ`. The const-name fragment for a port/param (non-alphanumerics become `_`).
pub fn screaming(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// `my_op` -> `MyOp`. The struct name for an operator's `type_name`.
pub fn struct_name(type_name: &str) -> String {
    type_name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut c = s.chars();
            match c.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + c.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// `MyOp` -> `my_op`. The default `type_name` for a struct, used by `operator_contract!` when the
/// author gives no explicit `type_name`. Operators whose wire name diverges from their struct
/// (e.g. `SamplePlayer` is `"sample"`) pass an explicit `type_name` and bypass this.
pub fn type_name_from_struct(struct_name: &str) -> String {
    let mut out = String::new();
    for (i, c) in struct_name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screaming_uppercases_and_sanitises() {
        assert_eq!(screaming("freq"), "FREQ");
        assert_eq!(screaming("in_min"), "IN_MIN");
    }

    #[test]
    fn struct_name_is_pascal_case() {
        assert_eq!(struct_name("my_op"), "MyOp");
        assert_eq!(struct_name("oscillator"), "Oscillator");
        assert_eq!(struct_name("sample"), "Sample");
    }

    #[test]
    fn type_name_from_struct_is_snake_case() {
        assert_eq!(type_name_from_struct("Oscillator"), "oscillator");
        assert_eq!(type_name_from_struct("SamplePlayer"), "sample_player");
        assert_eq!(type_name_from_struct("Add"), "add");
    }
}

//! The operator palette: every Operator a node can be added as, with a filter to type into.

use eframe::egui;
use reuben_api::authoring::{describe_operators, DescribeOperators, OperatorInfo};

/// The most rows the palette lists at once; typing narrows the rest.
const SHOWN: usize = 40;

pub struct Palette {
    /// Type names, in registry order.
    types: Vec<String>,
    filter: String,
}

impl Palette {
    pub fn new() -> Self {
        let operators = describe_operators(&DescribeOperators::default())
            .ok()
            .and_then(|answer| answer.output.operators)
            .unwrap_or_default();
        Self {
            types: addable(&operators),
            filter: String::new(),
        }
    }

    /// The type names `filter` matches, in registry order.
    fn matching(&self) -> Vec<&str> {
        let filter = self.filter.trim().to_lowercase();
        self.types
            .iter()
            .map(String::as_str)
            .filter(|name| name.contains(&filter))
            .collect()
    }

    /// The palette's contents, for a menu. Returns the type picked, which closes the menu.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<String> {
        let field = ui.add(
            egui::TextEdit::singleline(&mut self.filter)
                .hint_text("Filter operators")
                .desired_width(180.0),
        );
        if !field.has_focus() && self.filter.is_empty() {
            field.request_focus();
        }
        let matching = self.matching();
        let first = matching.first().map(|name| name.to_string());
        if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            if let Some(first) = first {
                return self.picked(ui, first);
            }
        }
        let mut picked = None;
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .show(ui, |ui| {
                for name in matching.iter().take(SHOWN) {
                    if ui.button(*name).clicked() {
                        picked = Some(name.to_string());
                    }
                }
                if matching.len() > SHOWN {
                    ui.weak(format!("{} more", matching.len() - SHOWN));
                }
                if matching.is_empty() {
                    ui.weak("No operator matches");
                }
            });
        picked.and_then(|name| self.picked(ui, name))
    }

    fn picked(&mut self, ui: &egui::Ui, name: String) -> Option<String> {
        self.filter.clear();
        ui.close();
        Some(name)
    }
}

/// The operators a node can be added as from the palette: those that need no resource. A
/// `voicer`, `subpatch` or sample player names its document or sample in the same write that adds
/// it, which a click on a type name cannot supply.
fn addable(operators: &[OperatorInfo]) -> Vec<String> {
    operators
        .iter()
        .filter(|op| op.resources.is_empty())
        .map(|op| op.type_name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_palette_offers_the_operators_that_need_no_resource() {
        let palette = Palette::new();
        for name in ["oscillator", "filter", "output", "lfo"] {
            assert!(palette.types.iter().any(|t| t == name), "{name}");
        }
        for name in ["voicer", "subpatch", "sample"] {
            assert!(!palette.types.iter().any(|t| t == name), "{name}");
        }
    }

    #[test]
    fn the_filter_matches_any_part_of_a_name_in_any_case() {
        let mut palette = Palette::new();
        palette.filter = " OSC".into();
        let matching = palette.matching();
        assert!(matching.contains(&"oscillator"));
        assert!(matching.contains(&"osc_out"));
        assert!(!matching.contains(&"filter"));
        palette.filter.clear();
        assert_eq!(palette.matching().len(), palette.types.len());
    }
}

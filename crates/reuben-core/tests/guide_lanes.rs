//! Tier-1 lane-tag cut test (the R7 acceptance): the authoring guide's web
//! slice is a deterministic function of the `lanes:` tags + the guide — every section
//! tagged, checkout-only sections dropped, kept sections verbatim and in source order,
//! never hand-paraphrased. `include_str!` pins the test to the committed guide,
//! so editing `docs/agents/authoring.md` re-runs it: a new heading without a tag (or with
//! a typo'd lane) fails here instead of silently skipping the web cut.

use reuben_core::guide::{lane_cut, Lane};

const GUIDE: &str = include_str!("../../../docs/agents/authoring.md");

/// The staleness guard: every heading in the committed guide carries a well-formed
/// `lanes:` tag, so the cut is total — for every lane, not just web.
#[test]
fn every_section_is_tagged_for_every_lane() {
    for lane in Lane::ALL {
        if let Err(e) = lane_cut(GUIDE, lane) {
            panic!("the guide must stay fully `lanes:`-tagged: {e}");
        }
    }
}

/// The web cut drops the checkout-only sections — the filesystem sample workflow (no disk
/// on the web lane) and the rules-corpus pointer — while the checkout lanes keep them.
#[test]
fn web_cut_drops_the_checkout_only_sections() {
    let web = lane_cut(GUIDE, Lane::Web).expect("tagged guide cuts clean");
    for checkout_only in ["## The sample workflow", "## Where the rules live"] {
        assert!(
            !web.contains(checkout_only),
            "checkout-only section {checkout_only:?} must not ship in the web cut"
        );
        for lane in [Lane::Skills, Lane::Mcp] {
            let cut = lane_cut(GUIDE, lane).expect("tagged guide cuts clean");
            assert!(
                cut.contains(checkout_only),
                "{checkout_only:?} must still ship on the {} lane",
                lane.name()
            );
        }
    }
    assert!(
        web.len() < GUIDE.len(),
        "the web cut must actually drop something — full-guide bundling is not the contract"
    );
}

/// Mechanically sliced, never paraphrased: every line of the web cut is a guide line,
/// verbatim and in source order (headings modulo the stripped tag comment).
#[test]
fn web_cut_is_verbatim_guide_content_in_order() {
    let web = lane_cut(GUIDE, Lane::Web).expect("tagged guide cuts clean");
    let mut guide_lines = GUIDE.lines();
    for cut_line in web.lines() {
        let matched = guide_lines.by_ref().any(|src| {
            src == cut_line
                || (src.contains("<!--")
                    && src[..src.rfind("<!--").unwrap()].trim_end() == cut_line)
        });
        assert!(
            matched,
            "web-cut line not found in the guide (in order): {cut_line:?}"
        );
    }
}

/// The cut is a pure function of tags + guide: same input, same bytes.
#[test]
fn cut_is_deterministic() {
    for lane in Lane::ALL {
        assert_eq!(
            lane_cut(GUIDE, lane).expect("tagged guide cuts clean"),
            lane_cut(GUIDE, lane).expect("tagged guide cuts clean"),
            "lane cut must be byte-stable for {}",
            lane.name()
        );
    }
}

/// Loop conduct is shared sauce: the authoring-loop section — including the
/// validate-proves-legal-not-audible check and describe-don't-infer — ships in every lane.
#[test]
fn loop_conduct_ships_in_every_lane() {
    for lane in Lane::ALL {
        let cut = lane_cut(GUIDE, lane).expect("tagged guide cuts clean");
        assert!(
            cut.contains("## The authoring loop"),
            "the authoring-loop section must ship on the {} lane",
            lane.name()
        );
        assert!(
            cut.contains("Sanity-check that it's audible") && cut.contains("never infer"),
            "loop conduct must ship on the {} lane",
            lane.name()
        );
    }
}

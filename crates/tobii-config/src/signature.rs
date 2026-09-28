//! What this project has measured about NaturalPoint's signature check.
//!
//! # Why this is here rather than beside the installer
//!
//! A TrackIR game finds its client DLL through a registry key and loads
//! whatever it names. NaturalPoint's client answers a signature challenge that
//! a clean-room DLL cannot; ours therefore cannot, and the material that would
//! answer it is theirs. That fact decides whether a game can receive anything
//! at all, so more than one screen has to state it: `tobii bridge install`
//! says it in a terminal, and the hub's game-setup window says it to somebody
//! who never opens one.
//!
//! Two surfaces saying one thing is how this project has repeatedly ended up
//! with two answers to one question — a wording fix applied to one of them and
//! not the other, found only when a reviewer read both. `tobii-cli` is a
//! `[[bin]]`, so the hub cannot link it; `tobii-config` is what both already
//! depend on, which makes it the one place both can ask.
//!
//! # Measurements, not a rule
//!
//! [`MEASURED`] is a list of what happened to named titles on named dates. It
//! is deliberately not a predicate: two titles are not a rule about the rest,
//! and no caller may render this as "your game will not work". What a third
//! title does is unknown until somebody runs it, and the honest shape of that
//! is a list that can grow rather than a boolean that cannot.

/// One title, put to the signature check, and what it did.
///
/// `behaviour` is a clause that completes "…", so it reads under the title in
/// a list and inside a sentence alike. It says what was observed and nothing
/// about what it implies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Measured {
    /// The game, as its store spells it.
    pub title: &'static str,
    /// Its Steam app id, where it was a Steam title.
    pub appid: Option<&'static str>,
    /// What it was running under, where that was recorded.
    pub runtime: Option<&'static str>,
    /// When it was measured, ISO 8601.
    pub date: &'static str,
    /// What it did, as a clause following the title.
    pub behaviour: &'static str,
}

/// Every title this project has watched meet the check.
///
/// Both stop at it, and they stop differently — which is the reason this is a
/// list of observations rather than a sentence saying "games reject us". A
/// reader deciding what to try needs to know that one gave up and one never
/// did.
pub const MEASURED: [Measured; 2] = [
    Measured {
        title: "Star Citizen",
        appid: None,
        runtime: None,
        date: "2026-08-15",
        behaviour: "rejects ours and never asks for data again",
    },
    Measured {
        title: "Microsoft Flight Simulator 2024",
        appid: Some("2537590"),
        runtime: Some("Proton Experimental"),
        date: "2026-09-27",
        behaviour: "calls the check 104 times in 1m45s, calls nothing else at all, and goes on \
                    retrying for as long as it runs",
    },
];

impl Measured {
    /// The title with what identifies it, as a sentence opens: *Microsoft
    /// Flight Simulator 2024 (Steam appid 2537590, Proton Experimental,
    /// 2026-09-27)*.
    pub fn named(&self) -> String {
        let mut inside = Vec::new();
        if let Some(appid) = self.appid {
            inside.push(format!("Steam appid {appid}"));
        }
        if let Some(runtime) = self.runtime {
            inside.push(runtime.to_string());
        }
        inside.push(self.date.to_string());
        format!("{} ({})", self.title, inside.join(", "))
    }
}

/// Why our own client cannot answer, what was measured, and what does work —
/// as flowing prose with no wrapping of its own.
///
/// Unwrapped on purpose. A GTK label wraps itself and a terminal caller knows
/// its own width and indentation, so the one thing this must not do is choose
/// for either of them. Callers that need a hanging indent apply it to what
/// they get back.
///
/// It states the gate, the measurements, that two titles are not a rule, and
/// the two routes that do carry data. It does **not** name a flag: which
/// spelling installs which client is the installer's business, and a window
/// with no flags should not be quoting them.
pub fn trackir_gate() -> String {
    let measured: Vec<String> = MEASURED
        .iter()
        .map(|m| format!("{} {}.", m.named(), m.behaviour))
        .collect();
    format!(
        "A TrackIR game loads whichever client DLL the registry names, and checks its \
         signature against NaturalPoint's. Ours cannot answer that, and the material that \
         would answer it is theirs: reproducing it is not this project's to do. \
         {count} ever measured against that check stop at it. {} \
         Two titles are not a rule about the rest, and nothing here knows what your game \
         does. An installed opentrack provides a client that does answer the check, and it \
         reads the same shared mapping, so our tracking can sit behind it. FreeTrack has no \
         signature check at all, so a game that speaks FreeTrack works with ours today.",
        measured.join(" "),
        count = count_word(MEASURED.len()),
    )
}

/// How many titles have been measured, in English, as a sentence opens.
///
/// In the paragraph rather than beside it: a hand-typed "Both" is what goes
/// stale the day a third measurement lands, and the sentence is the thing a
/// user reads.
fn count_word(n: usize) -> &'static str {
    match n {
        0 => "No title has been",
        1 => "The one title",
        2 => "Both titles",
        3 => "All three titles",
        _ => "Every title",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rendering that both surfaces will use has to name every measurement
    /// it has, or the list grows and one of the two screens goes on saying
    /// "both titles" about three.
    ///
    /// Asserts the count word and every title, so adding a third `Measured`
    /// without rewording fails here rather than in front of a user.
    #[test]
    fn the_gate_names_every_title_that_was_measured() {
        let text = trackir_gate();
        for m in MEASURED {
            assert!(text.contains(m.title), "{} is missing:\n{text}", m.title);
            assert!(text.contains(m.date), "{} has no date:\n{text}", m.title);
            assert!(
                text.contains(m.behaviour),
                "{} says nothing about what it did:\n{text}",
                m.title
            );
        }
        assert!(
            text.contains(&format!("{} ever measured", count_word(MEASURED.len()))),
            "the count and the list have to agree:\n{text}"
        );
    }

    /// What the paragraph must not become. It reports observations; a reader
    /// deciding whether to try their own game is entitled to be told that two
    /// results are not a rule, and a sentence predicting theirs would be this
    /// project claiming a measurement nobody took.
    #[test]
    fn the_gate_does_not_predict_what_an_unmeasured_game_will_do() {
        let text = trackir_gate();
        assert!(
            text.contains("Two titles are not a rule"),
            "it says so outright:\n{text}"
        );
        for claim in ["will not work", "cannot work", "no game", "every game"] {
            assert!(
                !text.contains(claim),
                "{claim:?} is a claim about games nobody ran:\n{text}"
            );
        }
    }

    /// It says what *does* carry data. A paragraph that only refused would
    /// leave a reader with nothing to do, and both routes named here are ones
    /// this project has shipped.
    #[test]
    fn the_gate_names_both_routes_that_carry_data() {
        let text = trackir_gate();
        assert!(text.contains("opentrack"), "the answering client:\n{text}");
        assert!(text.contains("FreeTrack"), "the ungated ABI:\n{text}");
    }

    /// No wrapping and no indentation: the callers differ, and a newline
    /// chosen here is one neither of them can take out.
    #[test]
    fn the_gate_leaves_the_wrapping_to_whoever_prints_it() {
        let text = trackir_gate();
        assert!(!text.contains('\n'), "no line breaks of its own:\n{text}");
        assert!(!text.contains("  "), "no run of spaces:\n{text}");
    }

    /// `named` carries whatever identifies a title and nothing it has not got,
    /// so a future measurement of a non-Steam title does not render an empty
    /// bracket.
    #[test]
    fn a_title_is_named_by_what_is_known_about_it() {
        let sc = MEASURED[0].named();
        assert_eq!(sc, "Star Citizen (2026-08-15)", "no empty fields: {sc}");
        let msfs = MEASURED[1].named();
        assert_eq!(
            msfs,
            "Microsoft Flight Simulator 2024 (Steam appid 2537590, Proton Experimental, \
             2026-09-27)"
        );
    }
}

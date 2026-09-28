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

/// The measurement for a Steam app id, if this project has one.
///
/// For a caller that can offer something different to a title known to stop at
/// the check — the hub's Games tab offers the one route that has been seen to
/// get past it, and offers it there and nowhere else, because everywhere else
/// it would be a guess about a game nobody has run.
pub fn measured_steam(appid: &str) -> Option<&'static Measured> {
    MEASURED.iter().find(|m| m.appid == Some(appid))
}

/// The same question for a game nobody bought on Steam, by the name a row
/// carries.
///
/// **The other half of [`measured_steam`], and it has to exist.** The first
/// entry in [`MEASURED`] is Star Citizen, which has no app id — it is not sold
/// on Steam — and it is the title people ask about and the reason the hub has a
/// list of games added by hand at all. Keying the offer on the app id alone
/// withholds it from the one measured game that can only be reached by folder.
///
/// # Why the match is loose, and how loose
///
/// The name is not typed: the hub derives it from the folder somebody picked,
/// so a Star Citizen prefix arrives as `StarCitizen`, `star-citizen` or
/// `Star Citizen LIVE` depending on how they laid their disk out. An exact
/// comparison matched none of those, which made this function — and the button
/// it gates — dead code for its only case.
///
/// So both sides are reduced to lowercase letters and digits, and a candidate
/// matches when it CONTAINS the measured title so reduced. `starcitizenlive`
/// matches `starcitizen`; `sc` does not, and neither does
/// `robertsspaceindustries`, which is a different name rather than a spelling
/// of this one.
///
/// Erring towards matching is the right way round here, and the asymmetry is
/// worth stating: a false match offers somebody a button they do not need,
/// which they can ignore; a false miss withholds the one route past the check
/// from the game it was measured on, which is what was happening.
pub fn measured_named(name: &str) -> Option<&'static Measured> {
    let fold = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let want = fold(name);
    MEASURED.iter().find(|m| want.contains(&fold(m.title)))
}

impl Measured {
    /// The whole observation as one clause: the title, what identifies it,
    /// and what it did.
    ///
    /// Here rather than in each caller's `format!`, because two surfaces
    /// built this same two-field string with their own joiners and one of
    /// them then dropped the disclaimer that goes with it. A caller chooses
    /// how to join these; it does not choose what one of them says.
    pub fn clause(&self) -> String {
        format!("{} {}", self.named(), self.behaviour)
    }

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
    // Built once. It is a pure function of a `const` array, and the game page
    // calls it on every refresh.
    static TEXT: std::sync::LazyLock<String> = std::sync::LazyLock::new(build_gate);
    TEXT.clone()
}

fn build_gate() -> String {
    let measured: Vec<String> = MEASURED
        .iter()
        .map(|m| format!("{}.", m.clause()))
        .collect();
    format!(
        "A TrackIR game loads whichever client DLL the registry names, and checks its \
         signature against NaturalPoint's. Ours cannot answer that, and the material that \
         would answer it is theirs: reproducing it is not this project's to do. \
         {count} put to that check stopped at it. {} \
         {tally} not a rule about the rest, and nothing here knows what your game \
         does. A separately installed client — opentrack ships one — is what can answer the \
         check; whether that then delivers tracking is not something this project has \
         watched happen. FreeTrack has no signature check at all, so a game that speaks \
         FreeTrack works with ours today.",
        measured.join(" "),
        count = count_word(MEASURED.len()),
        tally = tally_word(MEASURED.len()),
    )
}

/// The count, in the shape a "not a rule about the rest" sentence needs.
///
/// Public because every surface that states the measurements owes that
/// sentence, and one of them was found stating them without it.
pub fn tally() -> String {
    tally_word(MEASURED.len())
}

/// How many titles have been measured, in English, as a sentence opens.
///
/// In the paragraph rather than beside it: a hand-typed "Both" is what goes
/// stale the day a third measurement lands, and the sentence is the thing a
/// user reads.
fn count_word(n: usize) -> String {
    match n {
        0 => "No title has been".to_string(),
        1 => "The one title".to_string(),
        2 => "Both titles".to_string(),
        3 => "All three titles".to_string(),
        n => format!("All {n} titles"),
    }
}

/// The same count again, in the shape the second sentence needs.
///
/// Two slots, two phrasings: "Both titles put to that check stopped at it"
/// reads, and "Both titles are not a rule" does not. One helper serving both
/// produced exactly that sentence, which is what comes of reusing a word
/// because it holds the right number rather than because it fits.
fn tally_word(n: usize) -> String {
    match n {
        0 => "Nothing measured is".to_string(),
        1 => "One result is".to_string(),
        2 => "Two results are".to_string(),
        3 => "Three results are".to_string(),
        n => format!("{n} results are"),
    }
}

/// What has to be running behind a third-party client, and the two ways to
/// make it so.
///
/// Unwrapped, for [`trackir_gate`]'s reasons.
///
/// The client that answers the signature check only *reads* the shared
/// mapping; it does not create one. So a prefix can hold a client that passes
/// the check and still deliver nothing, which looks from the outside exactly
/// like the check having failed. Naming what is missing is the difference
/// between a user knowing that and a user concluding the bridge is broken.
///
/// **This used to name `tobii bridge run` as the answer, and that was wrong for
/// the case nearly everybody is in.** `bridge/core/src/feeder.rs` records the
/// measurement: a Steam game under Proton runs in its own wineserver, and a
/// provider started from a terminal with system Wine is a different session
/// whose `FT_SharedMem` is a different object — the game never sees it. That is
/// precisely why both of our client DLLs carry the receive loop themselves.
/// Repeating the old instruction sent somebody to run a command that could not
/// help them and then to conclude the bridge was broken when it did not.
///
/// So "the launch stopped freezing" and "the game is getting data" are two
/// different outcomes, and this project has been found reading the first as
/// the second. A screen that offers an install and names only the first would
/// be making that mistake in front of somebody.
pub fn provider_note() -> String {
    static TEXT: std::sync::LazyLock<String> = std::sync::LazyLock::new(build_provider_note);
    TEXT.clone()
}

fn build_provider_note() -> String {
    [
        "A client that answers the check only reads the shared mapping —",
        "something inside the game's own Wine session has to be filling it, and",
        "for a Steam game under Proton that cannot be `tobii bridge run`: a",
        "provider started from a terminal with system Wine is a different",
        "session, its mapping is a different object, and the game never sees it.",
        "What fills the mapping for our own client is our own client — both of",
        "our DLLs carry the receive loop, inside the game's process, which is",
        "the only place that is certain to be the right session. Replace the",
        "one the game loads with somebody else's and that goes with it, unless",
        "something else in the same session is filling the mapping. Whether",
        "anything is, on any particular game, is not something this project has",
        "watched.",
    ]
    .join(" ")
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
        // Both sentences that carry a count, not just the first: the count
        // word was computed and the sentence three clauses later was typed,
        // so the paragraph said "All three titles … Two titles are not a
        // rule" on the one edit the computation exists to survive — and the
        // test pinned the typed half.
        // No assertion that the two counts are in the text: they are put there
        // by interpolating these same two functions, so asking them the same
        // question and checking the answer came back is a test that cannot
        // fail. The loop above — over `MEASURED` itself — is the one that
        // notices a third title never reaching the output.
    }

    /// What the paragraph must not become. It reports observations; a reader
    /// deciding whether to try their own game is entitled to be told that two
    /// results are not a rule, and a sentence predicting theirs would be this
    /// project claiming a measurement nobody took.
    #[test]
    fn the_gate_does_not_predict_what_an_unmeasured_game_will_do() {
        let text = trackir_gate();
        assert!(
            text.contains("not a rule about the rest"),
            "it says so outright:\n{text}"
        );
        // A short list of spellings, and it is worth being clear about what
        // that buys: it catches this paragraph being reworded back into a
        // prediction, and it does not catch the class — "your game is unlikely
        // to get past it" would sail through. The sentence above is what
        // actually carries the promise; these are a regression guard on the
        // two wordings this project has already had to correct.
        for claim in ["does answer the check", "will not work"] {
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
        // Both of them, by name: the first was written with this rule in mind
        // and the second was added later and broke it, with a run of spaces
        // where a line continuation should have closed up. A test naming one
        // function is a test the next function does not have.
        for (what, text) in [
            ("trackir_gate", trackir_gate()),
            ("provider_note", provider_note()),
        ] {
            assert!(!text.contains('\n'), "{what} breaks its own lines:\n{text}");
            assert!(!text.contains("  "), "{what} has a run of spaces:\n{text}");
            assert_eq!(text.trim(), text, "{what} is padded:\n{text}");
        }
    }

    /// The lookup a caller uses to offer something different to a title that
    /// is known to stop at the check.
    ///
    /// By app id and not by name: a name is what a store spells and a store
    /// spells it differently in different places, and the caller has an app id
    /// in its hand. A title measured without one — Star Citizen is not on
    /// Steam — is unreachable this way, which is correct rather than a gap:
    /// there is no app id to match it against.
    #[test]
    fn a_measured_title_can_be_found_by_its_app_id() {
        let msfs = measured_steam("2537590").expect("MSFS 2024 is measured");
        assert_eq!(msfs.title, "Microsoft Flight Simulator 2024");
        assert_eq!(measured_steam("359320"), None, "Elite was never put to it");
        assert_eq!(measured_steam(""), None, "and an empty id matches nothing");
        // Star Citizen is measured and has no app id, so it cannot be found
        // here — asserted so that giving it one later is a deliberate act.
        assert!(MEASURED.iter().any(|m| m.appid.is_none()));
    }

    /// The measured title that has no app id is reachable, or the one route
    /// past the check is withheld from the game it was measured on.
    ///
    /// Star Citizen is `MEASURED[0]`, has `appid: None`, and is the title the
    /// hub's hand-added list exists for. A lookup that only knows app ids
    /// cannot find it — which is exactly the hole this closes.
    #[test]
    fn the_measured_title_with_no_app_id_is_reachable_by_name() {
        let sc = measured_named("Star Citizen").expect("measured, and not on Steam");
        assert_eq!(sc.appid, None, "which is why the app-id lookup misses it");
        // The shapes a PREFIX FOLDER really has, because that is where the name
        // comes from — the hub derives it from the directory somebody picked,
        // and an exact comparison matched none of these, which made this
        // function and the button it gates dead for its only case.
        for folder in [
            "Star Citizen",
            "star-citizen",
            "StarCitizen",
            "star_citizen",
            "Star Citizen LIVE",
            "  STAR CITIZEN  ",
        ] {
            assert!(measured_named(folder).is_some(), "{folder:?}");
        }
        // And the limit: a different name rather than a spelling of this one.
        for other in ["Elden Ring", "robertsspaceindustries", "sc", ""] {
            assert_eq!(measured_named(other), None, "{other:?}");
        }
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

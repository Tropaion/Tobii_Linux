//! Games this machine has that Steam does not list: a name, and the Wine
//! prefix they live in.
//!
//! # Why there is a list at all
//!
//! `tobii bridge install` has always taken `--prefix PATH` as well as
//! `--steam <appid>`, so a non-Steam game has never been out of reach from a
//! terminal. What was out of reach was *finding it again*: the hub's Games tab
//! is built from Steam's manifests, so a title Steam has never heard of — Star
//! Citizen is the one people ask about — could not be picked, and the prefix
//! had to be retyped every time. This is the file that remembers it.
//!
//! # The format, and why it is not TOML
//!
//! One game per line: a name, a tab, and a path. Lines starting with `#` and
//! blank lines are comments.
//!
//! ```text
//! # tobii-linux custom games
//! Star Citizen<TAB>/home/me/Games/star-citizen/pfx
//! ```
//!
//! (`<TAB>` is a literal tab. Written out because a real one in a doc comment
//! is a lint, and because a reader who copies this line wants to know that the
//! whitespace between the two fields is not spaces.)
//!
//! Two fields, both of them free text, is not a grammar worth a parser. The
//! profiles format next door is line-based and hand-read for the same reason,
//! and it has to carry nested tables; this does not. What a quoted format would
//! buy is a name or a path containing a tab, and what it would cost is an
//! escaping rule in a reader, a writer and a test — so [`save_to`] refuses
//! those two characters instead, which is one rule in one place and is
//! reportable to the person typing the name.
//!
//! # What this promises about the paths in it
//!
//! Nothing. A path here is what somebody typed or picked, and it is stored
//! whether or not it exists: a game on a drive that is not plugged in is
//! exactly the case the Games tab's third group exists for, and a list that
//! dropped an entry because the disk was absent would lose the entry the first
//! time somebody unplugged it. [`CustomGame::exists`] is how a caller asks,
//! and it is the caller's job to say so rather than this module's to prune.

use std::path::{Path, PathBuf};

use crate::paths;

/// One game somebody added by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomGame {
    /// What to call it. Display only — nothing matches on it.
    pub name: String,
    /// The Wine prefix. This is the identity: two entries naming one prefix
    /// are one game however they are spelled, because it is the prefix that
    /// `tobii bridge install --prefix` writes into and two rows pointing at it
    /// would be two rows that cannot disagree about anything that matters.
    pub prefix: PathBuf,
}

impl CustomGame {
    /// Whether the prefix is a directory that is there right now.
    ///
    /// Not consulted by the reader or the writer — see the module docs. A
    /// caller shows the difference; it does not act on it.
    pub fn exists(&self) -> bool {
        self.prefix.is_dir()
    }
}

/// Why a game could not be added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddError {
    /// The name is empty, or is only whitespace.
    NoName,
    /// A tab or a newline, in the name or in the path. See the module docs for
    /// why these two and nothing else.
    Untypeable(&'static str),
    /// A prefix already in the list. Carries the name it is under, because the
    /// commonest way to hit this is adding the same game twice under two
    /// spellings and the useful answer is which one is already there.
    Already(String),
}

impl std::fmt::Display for AddError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AddError::NoName => f.write_str("give the game a name"),
            AddError::Untypeable(what) => {
                write!(
                    f,
                    "the {what} contains a tab or a line break, which this list cannot hold"
                )
            }
            AddError::Already(name) => {
                write!(
                    f,
                    "that prefix is already in the list, as \u{201c}{name}\u{201d}"
                )
            }
        }
    }
}

impl std::error::Error for AddError {}

/// The file: `<config dir>/custom-games.tsv`.
pub fn path() -> PathBuf {
    paths::config_dir().join(paths::CUSTOM_GAMES)
}

/// Read the list, in the order the file gives.
///
/// A file that is not there is an empty list — nobody has added a game — and
/// so is a file that cannot be read. This is a convenience, not a setting:
/// refusing to show the Games tab because a list of nicknames would not open
/// would be the tail wagging the dog, and the failure is visible the moment
/// somebody looks for a game that is not in it.
pub fn list() -> Vec<CustomGame> {
    list_from(&path())
}

/// [`list`] over a given file.
pub fn list_from(file: &Path) -> Vec<CustomGame> {
    let Ok(bytes) = std::fs::read(file) else {
        return Vec::new();
    };
    parse(&bytes)
}

/// Every line of `text` that is a game, in order, without repeats.
///
/// A line this program did not write is skipped in silence, and that is the
/// one place this module is deliberately quieter than [`crate::profiles`] is
/// about its own directory: a profile that will not parse is a thing somebody
/// wrote and expects to be applied, and a junk line here is a list of
/// nicknames with a typo in it. What a caller can act on is the games; what it
/// cannot act on is a line with three tabs in it.
pub fn parse(bytes: &[u8]) -> Vec<CustomGame> {
    use std::os::unix::ffi::OsStrExt;
    let mut out: Vec<CustomGame> = Vec::new();
    for raw in bytes.split(|b| *b == b'\n') {
        let line = match raw.split_last() {
            Some((b'\r', head)) => head,
            _ => raw,
        };
        let text = String::from_utf8_lossy(line);
        if text.trim().is_empty() || text.trim_start().starts_with('#') {
            continue;
        }
        // Split once. A second tab means a line this writer did not produce,
        // and guessing which of the three fields is the path is exactly the
        // guess that puts an install somewhere nobody asked for.
        let mut parts = line.splitn(2, |b| *b == b'\t');
        let (Some(name), Some(prefix)) = (parts.next(), parts.next()) else {
            continue;
        };
        if prefix.contains(&b'\t') {
            continue;
        }
        // The name is a person's typing and must be text; the path is bytes and
        // is kept as they are, trimmed of ASCII space only — a trailing space in
        // a directory name is legal, and stripping it pointed the entry at a
        // directory that does not exist.
        let Ok(name) = std::str::from_utf8(name) else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || prefix.is_empty() {
            continue;
        }
        let game = CustomGame {
            name: name.to_string(),
            prefix: PathBuf::from(std::ffi::OsStr::from_bytes(prefix)),
        };
        // First spelling wins, and the identity is the prefix — see
        // [`CustomGame::prefix`].
        if !out.iter().any(|g| g.prefix == game.prefix) {
            out.push(game);
        }
    }
    out
}

/// Render the list as the file holds it.
///
/// Paths are written through [`std::os::unix::ffi::OsStrExt`] rather than
/// `to_string_lossy`, which replaces every byte it cannot decode with U+FFFD
/// — so a prefix on a path that is not valid UTF-8, which Linux allows, came
/// back from the next read pointing at a directory that does not exist, no
/// longer matched by `remove`, and no longer recognised as a duplicate by
/// `add`. The file is bytes and a path is bytes; the only thing in between
/// that has to be UTF-8 is the name, which a person typed.
pub fn render(games: &[CustomGame]) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let mut out = Vec::from(HEADER.as_bytes());
    for g in games {
        out.extend_from_slice(g.name.as_bytes());
        out.push(b'\t');
        out.extend_from_slice(g.prefix.as_os_str().as_bytes());
        out.push(b'\n');
    }
    out
}

/// What the file starts with, so a person who opens it knows what it is.
const HEADER: &str = "# tobii-linux custom games\n\
                      # One game per line: a name, a TAB, and the Wine prefix it lives in.\n\
                      # `tobii bridge install --prefix <that path>` is what the hub runs for one.\n";

/// Write the list.
pub fn save(games: &[CustomGame]) -> std::io::Result<()> {
    save_to(&path(), games)
}

/// [`save`] to a given file.
pub fn save_to(file: &Path, games: &[CustomGame]) -> std::io::Result<()> {
    crate::write_atomic(file, &render(games))
}

/// Add one game to `games`, or say why not.
///
/// Here rather than in the caller because all three refusals are about the
/// FILE — what it can hold and what is already in it — and a window that
/// enforced them itself would be a second opinion about a format it does not
/// own. The caller's job is to show the sentence.
pub fn add(games: &mut Vec<CustomGame>, name: &str, prefix: &Path) -> Result<(), AddError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AddError::NoName);
    }
    if name.contains('\t') || name.contains('\n') {
        return Err(AddError::Untypeable("name"));
    }
    let shown = prefix.to_string_lossy();
    if shown.contains('\t') || shown.contains('\n') {
        return Err(AddError::Untypeable("folder"));
    }
    if let Some(had) = games.iter().find(|g| g.prefix == prefix) {
        return Err(AddError::Already(had.name.clone()));
    }
    games.push(CustomGame {
        name: name.to_string(),
        prefix: prefix.to_path_buf(),
    });
    Ok(())
}

/// Drop the entry for `prefix`, if there is one. Answers whether there was.
pub fn remove(games: &mut Vec<CustomGame>, prefix: &Path) -> bool {
    let before = games.len();
    games.retain(|g| g.prefix != prefix);
    games.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(name: &str, prefix: &str) -> CustomGame {
        CustomGame {
            name: name.to_string(),
            prefix: PathBuf::from(prefix),
        }
    }

    /// What this writes, it reads back — including the awkward characters a
    /// game's name really has.
    ///
    /// A round trip and not two assertions about a literal: the format has one
    /// writer and one reader, and a test that pinned the bytes would pass on
    /// the day they stop agreeing with each other.
    #[test]
    fn a_list_survives_being_written_and_read_again() {
        let games = vec![
            game("Star Citizen", "/home/me/Games/star citizen/pfx"),
            game("Sid Meier's Alpha Centauri", "/mnt/disk/ac/pfx"),
            game("F.E.A.R. — Director's Cut", "/srv/games/fear/pfx"),
        ];
        assert_eq!(parse(&render(&games)), games);
    }

    /// The header is a comment, so reading back what was written cannot grow
    /// an entry called `# tobii-linux custom games`.
    #[test]
    fn the_header_is_not_read_back_as_a_game() {
        assert!(parse(HEADER.as_bytes()).is_empty());
        assert!(render(&[]).starts_with(b"#"));
    }

    /// A line this writer did not produce is skipped, and the skip is silent.
    ///
    /// The three-field line is the one that matters: a caller that guessed
    /// which field was the path would be guessing where to install, and an
    /// install into a guessed directory is the one mistake this whole tab is
    /// written against.
    #[test]
    fn a_line_this_program_did_not_write_is_not_guessed_at() {
        let text = "# a comment\n\
                    \n\
                    Good\t/p/one\n\
                    three\tfields\there\n\
                    \tno name\n\
                    NoPrefix\t\n\
                    no tab at all\n\
                    Also good\t/p/two\n";
        assert_eq!(
            parse(text.as_bytes()),
            vec![game("Good", "/p/one"), game("Also good", "/p/two")]
        );
    }

    /// The prefix is the identity, so the same prefix twice is one game.
    ///
    /// Both halves: a file that holds it twice reads as one, and `add` refuses
    /// the second — naming the entry that is already there, because the
    /// commonest way to hit this is adding one game under two spellings and
    /// the useful answer is which spelling won.
    #[test]
    fn one_prefix_is_one_game_however_it_is_named() {
        let text = "First\t/p/one\nSecond\t/p/one\n";
        assert_eq!(parse(text.as_bytes()), vec![game("First", "/p/one")]);

        let mut games = parse(text.as_bytes());
        let err = add(&mut games, "Third", Path::new("/p/one")).expect_err("already there");
        assert_eq!(err, AddError::Already("First".to_string()));
        assert!(
            err.to_string().contains("First"),
            "the sentence names the one that is already there: {err}"
        );
        assert_eq!(games.len(), 1, "and nothing was added");
    }

    /// The two characters the format cannot hold are refused where somebody is
    /// typing, not written and lost on the way back.
    ///
    /// Without this, a name with a tab in it writes a line that reads back as
    /// a different game, and a name with a newline writes a line that reads
    /// back as two — one of which has a path for a name.
    #[test]
    fn a_name_or_a_folder_the_format_cannot_hold_is_refused_before_it_is_written() {
        let mut games = Vec::new();
        assert_eq!(
            add(&mut games, "a\tb", Path::new("/p/one")),
            Err(AddError::Untypeable("name"))
        );
        assert_eq!(
            add(&mut games, "ok", Path::new("/p/one\ttwo")),
            Err(AddError::Untypeable("folder"))
        );
        assert_eq!(
            add(&mut games, "a\nb", Path::new("/p/one")),
            Err(AddError::Untypeable("name"))
        );
        assert_eq!(
            add(&mut games, "   ", Path::new("/p/one")),
            Err(AddError::NoName)
        );
        assert!(games.is_empty(), "nothing got in: {games:?}");

        // And the ordinary case still works, with the name trimmed.
        assert_eq!(
            add(&mut games, "  Star Citizen  ", Path::new("/p/one")),
            Ok(())
        );
        assert_eq!(games, vec![game("Star Citizen", "/p/one")]);
    }

    /// Removing answers whether there was anything to remove, so a caller can
    /// tell "gone" from "was never there" rather than reporting success at a
    /// user who is looking at the row.
    #[test]
    fn removing_says_whether_it_removed_anything() {
        let mut games = vec![game("One", "/p/one"), game("Two", "/p/two")];
        assert!(remove(&mut games, Path::new("/p/one")));
        assert_eq!(games, vec![game("Two", "/p/two")]);
        assert!(!remove(&mut games, Path::new("/p/one")));
    }

    /// A path is bytes, and the round trip this module promises has to hold
    /// for the ones that are not text.
    ///
    /// Linux allows any byte but NUL and `/` in a path. `to_string_lossy`
    /// replaced every undecodable one with U+FFFD, so the entry came back
    /// pointing at a directory that does not exist, `remove` by the original
    /// path no longer matched it, and `add` of the same folder was no longer
    /// caught as a duplicate — three failures from one lossy conversion, none
    /// of them visible until somebody had such a path.
    #[test]
    fn a_path_that_is_not_utf8_survives_the_round_trip() {
        use std::os::unix::ffi::OsStrExt;
        let odd = PathBuf::from(std::ffi::OsStr::from_bytes(b"/games/\xff\xfe/pfx"));
        let g = CustomGame {
            name: "Bad Bytes".to_string(),
            prefix: odd.clone(),
        };
        let back = parse(&render(std::slice::from_ref(&g)));
        assert_eq!(back, vec![g], "the bytes came back as they went in");
        let mut games = back;
        assert_eq!(
            add(&mut games, "Again", &odd),
            Err(AddError::Already("Bad Bytes".to_string())),
            "and it is still recognised as the same prefix"
        );
        assert!(remove(&mut games, &odd), "and still removable by it");
    }

    /// A trailing space in a folder name is part of the name.
    ///
    /// Trimming the path made the entry point one directory away from the one
    /// the user picked. The NAME is trimmed — that is somebody's typing — and
    /// the path is not.
    #[test]
    fn a_path_keeps_its_own_whitespace_and_the_name_does_not() {
        let g = CustomGame {
            name: "Spacey".to_string(),
            prefix: PathBuf::from("/games/odd /pfx"),
        };
        assert_eq!(parse(&render(std::slice::from_ref(&g))), vec![g]);
        let mut games = Vec::new();
        assert_eq!(add(&mut games, "  Trimmed  ", Path::new("/p/one")), Ok(()));
        assert_eq!(games[0].name, "Trimmed");
    }

    /// A prefix that is not there is kept, not pruned.
    ///
    /// The Games tab's whole third group is games this machine cannot see
    /// right now, and a list that dropped an entry because a drive was
    /// unplugged would lose it the first time somebody unplugged it.
    #[test]
    fn a_prefix_that_is_not_on_this_machine_is_still_in_the_list() {
        let g = game("Gone", "/mnt/definitely-not-here/pfx");
        assert_eq!(parse(&render(std::slice::from_ref(&g))), vec![g.clone()]);
        assert!(!g.exists());
    }
}

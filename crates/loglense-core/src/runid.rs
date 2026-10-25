//! Run identifiers: `YYYYMMDD-<6 Crockford base32 chars>`.
//!
//! Format contract: 8 ASCII digits (a real calendar date), `-`, then 6 chars
//! from the Crockford base32 alphabet `0-9 A-H J K M N P-T V-Z` (no `I L O U`
//! so visually ambiguous glyphs never appear in ids users transcribe).

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use time::{Date, Month};

/// Number of create attempts before [`RunId::create_run_dir`] gives up.
const MAX_CREATE_TRIES: u8 = 5;

/// Total length of a run id: 8 date digits + `-` + 6 random chars.
const ID_LEN: usize = 15;

/// Crockford base32 alphabet (excludes `I`, `L`, `O`, `U`).
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Errors from parsing or creating [`RunId`]s.
#[derive(Debug, thiserror::Error)]
pub enum RunIdError {
    /// The id is not 15 characters long.
    #[error("invalid run id `{input}`: expected 15 chars `YYYYMMDD-XXXXXX`, found {len}")]
    InvalidLength {
        /// The rejected input.
        input: String,
        /// Its actual length.
        len: usize,
    },
    /// The id lacks the `-` separator between date and random part.
    #[error("invalid run id `{input}`: expected `-` at position 8")]
    MissingSeparator {
        /// The rejected input.
        input: String,
    },
    /// The id contains a character outside the allowed charset for its position.
    #[error("invalid run id `{input}`: invalid character `{ch}` at position {pos}")]
    InvalidCharacter {
        /// The rejected input.
        input: String,
        /// The offending character.
        ch: char,
        /// Its 0-based position.
        pos: usize,
    },
    /// The date part is not a valid calendar date.
    #[error("invalid run id `{input}`: `{date}` is not a valid calendar date")]
    InvalidDate {
        /// The rejected input.
        input: String,
        /// The date part that failed to parse.
        date: String,
    },
    /// Every create attempt collided with an existing run directory.
    #[error("run id collision: no free run directory under {root} after {tries} tries")]
    CollisionExhausted {
        /// The `runs/` directory searched.
        root: PathBuf,
        /// Attempts made.
        tries: u8,
    },
    /// Creating the run directory failed with an unexpected I/O error.
    #[error("creating run directory {path} failed: {source}")]
    DirCreate {
        /// The run directory that could not be created.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
}

/// A run identifier, ordered by `(date, random part)` lexicographically.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct RunId {
    id: String,
}

impl RunId {
    /// Builds an id for `run_date` from `entropy`.
    ///
    /// Only the low 30 bits of `entropy` are used (the random part is exactly
    /// 6 base32 chars = 30 bits); collisions are handled by
    /// [`RunId::create_run_dir`], not by the id itself.
    pub fn generate(run_date: Date, entropy: u32) -> RunId {
        let mut id = format!(
            "{:04}{:02}{:02}-",
            run_date.year(),
            u8::from(run_date.month()),
            run_date.day()
        );
        // 6 chars x 5 bits, least significant group emitted last.
        let mut value = entropy & 0x3FFF_FFFF;
        let mut chars = [b'0'; 6];
        for slot in chars.iter_mut().rev() {
            *slot = ALPHABET[(value & 0x1F) as usize];
            value >>= 5;
        }
        // Every alphabet byte is ASCII, so `as char` is lossless.
        let random: String = chars.iter().map(|&b| b as char).collect();
        id.push_str(&random);
        RunId { id }
    }

    /// The id text, e.g. `20261024-A3F8C2`.
    pub fn as_str(&self) -> &str {
        &self.id
    }

    /// Creates a fresh run directory named after a generated id under `root`.
    ///
    /// Draws entropy, tries `create_dir`; on `EEXIST` (another run grabbed the
    /// id first) draws again — at most [`MAX_CREATE_TRIES`] attempts, then
    /// [`RunIdError::CollisionExhausted`]. Returns the id and the created path.
    pub fn create_run_dir(
        root: &Path,
        run_date: Date,
        mut entropy: impl FnMut() -> u32,
    ) -> Result<(RunId, PathBuf), RunIdError> {
        for _ in 0..MAX_CREATE_TRIES {
            let id = RunId::generate(run_date, entropy());
            let dir = root.join(id.as_str());
            match std::fs::create_dir(&dir) {
                Ok(()) => return Ok((id, dir)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(RunIdError::DirCreate {
                        path: dir,
                        source: e,
                    })
                }
            }
        }
        Err(RunIdError::CollisionExhausted {
            root: root.to_path_buf(),
            tries: MAX_CREATE_TRIES,
        })
    }

    /// Validates `id` and returns the structured failure if it is malformed.
    fn validate(id: &str) -> Result<(), RunIdError> {
        if id.len() != ID_LEN {
            return Err(RunIdError::InvalidLength {
                input: id.to_string(),
                len: id.len(),
            });
        }
        let bytes = id.as_bytes();
        if bytes[8] != b'-' {
            return Err(RunIdError::MissingSeparator {
                input: id.to_string(),
            });
        }
        for pos in (0..8).chain(9..ID_LEN) {
            let b = bytes[pos];
            let ok = if pos < 8 {
                b.is_ascii_digit()
            } else {
                is_alphabet_byte(b)
            };
            if !ok {
                return Err(RunIdError::InvalidCharacter {
                    input: id.to_string(),
                    ch: char::from(b),
                    pos,
                });
            }
        }
        let year = two_digits(id, 0) * 100 + two_digits(id, 2);
        let month = two_digits(id, 4);
        let day = two_digits(id, 6);
        let date_valid = month_from(month)
            .and_then(|m| Date::from_calendar_date(year as i32, m, day as u8).ok())
            .is_some();
        if date_valid {
            Ok(())
        } else {
            Err(RunIdError::InvalidDate {
                input: id.to_string(),
                date: id[..8].to_string(),
            })
        }
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.id)
    }
}

impl FromStr for RunId {
    type Err = RunIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        RunId::validate(s).map(|()| RunId { id: s.to_string() })
    }
}

impl TryFrom<String> for RunId {
    type Error = RunIdError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::from_str(&s)
    }
}

impl From<RunId> for String {
    fn from(id: RunId) -> String {
        id.id
    }
}

/// Parses two ASCII digits at `at..at + 2` into `u32` (charset-checked first).
fn two_digits(id: &str, at: usize) -> u32 {
    let bytes = id.as_bytes();
    (u32::from(bytes[at] - b'0')) * 10 + u32::from(bytes[at + 1] - b'0')
}

/// Whether `b` is in the Crockford base32 alphabet.
fn is_alphabet_byte(b: u8) -> bool {
    matches!(
        b,
        b'0'..=b'9' | b'A'..=b'H' | b'J' | b'K' | b'M' | b'N' | b'P'..=b'T' | b'V'..=b'Z'
    )
}

/// Month from a 1-12 number, `None` when out of range.
fn month_from(number: u32) -> Option<Month> {
    Month::try_from(u8::try_from(number).ok()?).ok()
}

use std::ops::Range;

use nostr::prelude::*;

const BECH32_SEPARATOR: u8 = b'1';
const SCHEME_WITH_COLON: &str = "nostr:";

/// Nostr parsed token with its range in the original text
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// The parsed NIP-21 URI
    ///
    /// <https://github.com/nostr-protocol/nips/blob/master/21.md>
    pub value: Nip21,
    /// The range of this token in the original text
    pub range: Range<usize>,
}

#[derive(Debug, Clone, Copy)]
struct Match {
    start: usize,
    end: usize,
}

/// Nostr parser
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NostrParser;

impl Default for NostrParser {
    fn default() -> Self {
        Self::new()
    }
}

impl NostrParser {
    /// Create new parser
    pub const fn new() -> Self {
        Self
    }

    /// Parse text
    pub fn parse<'a>(&self, text: &'a str) -> NostrParserIter<'a> {
        NostrParserIter::new(text)
    }
}

struct FindMatches<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> FindMatches<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            bytes: text.as_bytes(),
            pos: 0,
        }
    }

    fn try_parse_nostr_uri(&mut self) -> Option<Match> {
        let start = self.pos;
        let bytes = self.bytes;
        let len = bytes.len();

        // Check if we have "nostr:" prefix
        if len - start < SCHEME_WITH_COLON.len() {
            return None;
        }

        // Check for "nostr:" prefix (case-insensitive)
        let scheme_prefix = &bytes[start..start + SCHEME_WITH_COLON.len()];
        if !scheme_prefix.eq_ignore_ascii_case(SCHEME_WITH_COLON.as_bytes()) {
            return None;
        }

        // Skip the scheme
        let pos = start + SCHEME_WITH_COLON.len();

        // Parse bech32 entity
        let mut has_separator = false;
        let mut end = pos;

        while end < len {
            let byte = bytes[end];

            // Check for bech32 separator
            if byte == BECH32_SEPARATOR && !has_separator {
                has_separator = true;
                end += 1;
                continue;
            }

            // Check if character is valid for bech32
            if !byte.is_ascii_alphanumeric() {
                break;
            }

            end += 1;
        }

        // Must have at least one character after separator
        if !has_separator || end <= pos + 1 {
            return None;
        }

        // Update position
        self.pos = end;

        Some(Match { start, end })
    }
}

impl Iterator for FindMatches<'_> {
    type Item = Match;

    fn next(&mut self) -> Option<Self::Item> {
        while self.pos < self.bytes.len() {
            // Try to parse nostr URI
            if let Some(mat) = self.try_parse_nostr_uri() {
                return Some(mat);
            }

            // Skip one character if no match found
            self.pos += 1;
        }

        None
    }
}

enum HandleMatch {
    Token(Token),
    Recursion,
}

pub struct NostrParserIter<'a> {
    /// The original text
    text: &'a str,
    /// Matches found
    matches: FindMatches<'a>,
    /// A pending match
    pending_match: Option<Match>,
    /// Last match end index
    last_match_end: usize,
}

impl<'a> NostrParserIter<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            matches: FindMatches::new(text),
            pending_match: None,
            last_match_end: 0,
        }
    }

    fn handle_match(&mut self, mat: Match) -> HandleMatch {
        // Update last match end
        self.last_match_end = mat.end;

        // Extract the matched string
        let data: &str = &self.text[mat.start..mat.end];

        // Parse NIP-21 URI
        match Nip21::parse(data) {
            Ok(uri) => HandleMatch::Token(Token {
                value: uri,
                range: mat.start..mat.end,
            }),
            // If the nostr URI parsing is invalid, skip it
            Err(_) => HandleMatch::Recursion,
        }
    }
}

impl<'a> Iterator for NostrParserIter<'a> {
    type Item = Token;

    fn next(&mut self) -> Option<Self::Item> {
        // Handle a pending match
        if let Some(pending_match) = self.pending_match.take() {
            return match self.handle_match(pending_match) {
                HandleMatch::Token(token) => Some(token),
                HandleMatch::Recursion => self.next(),
            };
        }

        match self.matches.next() {
            Some(mat) => {
                // Skip any text before this match
                if mat.start > self.last_match_end {
                    // Update pending match
                    // This will be handled at next iteration, in `handle_match` method.
                    self.pending_match = Some(mat);

                    // Skip the text before the match
                    self.last_match_end = mat.start;
                    return self.next();
                }

                // Handle match
                match self.handle_match(mat) {
                    HandleMatch::Token(token) => Some(token),
                    HandleMatch::Recursion => self.next(),
                }
            }
            None => None,
        }
    }
}

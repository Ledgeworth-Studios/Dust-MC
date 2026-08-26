//! `.mcfunction` files: read as lines, and no further.
//!
//! # What is decided here, and what pointedly is not
//!
//! A function file is a list of commands, one per line. The commands are
//! **opaque strings** at this layer: `execute as @e at @s run tp @s ~ ~-1 ~`
//! is stored byte-for-byte and never inspected. That is the same line the
//! crate draws against recipes — a second reader of one grammar is two
//! readers that disagree — but here it is sharper, because the command
//! grammar changes every release and half-parsing it would produce functions
//! that load and then do something subtly other than what vanilla would do
//! with the same file. Parsing commands into meaning belongs to the layer
//! that executes them; this module owns only the *file* rules.
//!
//! # The line rules, pinned down
//!
//! These are Minecraft's own rules for reading a function file, reproduced
//! exactly because a loader that disagrees with the game about where one
//! command ends and the next begins will run the wrong commands:
//!
//! * The file is UTF-8 text. Anything else is an error naming the first bad
//!   byte — the same rule, worded the same way, as [`crate::json`], because
//!   two containers saying "not UTF-8" in two voices teaches operators to
//!   skim both.
//! * A leading byte-order mark is tolerated, as in [`crate::json`]. Windows
//!   editors add it invisibly and the file is fine.
//! * Lines are split on `\n`; a trailing `\r` (the other half of CRLF) is
//!   dropped before anything else looks at the line.
//! * Each line is trimmed of surrounding whitespace. An empty result is
//!   skipped; so is any line whose first surviving character is `#`, which is
//!   how both vanilla and every pack on earth write comments. Leading
//!   whitespace before the `#` does not rescue it: trimming happens first,
//!   then the comment check, in that order.
//! * Whatever survives becomes one command, recorded with its **physical
//!   line number** — counting every line of the file, blanks and comments
//!   included — because that is the number an editor shows, and a diagnostic
//!   that names a different line than the editor does has failed at its one
//!   job.
//!
//! There is deliberately no line-length cap and no command-count cap beyond
//! the container's own [`crate::pack::MAX_FILE_BYTES`]: the parse allocates
//! in proportion to bytes already capped, so the cap that stops a hostile
//! zip entry is the same one stopping a hostile function file.
//!
//! # Provenance and overriding
//!
//! Functions overlay like everything else. Two packs defining
//! `minecraft:tick` are two answers to one question and the later pack wins;
//! the loser stays named in [`LoadedFunction::overridden`]. One pack can also
//! reach one name twice — the pre-1.21 `functions/` spelling alongside the
//! current one, which Minecraft itself would treat as two different worlds
//! and Dust merges into one namespace. That collision is reported rather
//! than resolved silently, and the copy under the current spelling wins.

use crate::finding::Finding;

/// One command of a function file, and the line it was written on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionLine {
    /// 1-based, counting every physical line of the file including blanks
    /// and comments. This is the number a text editor shows.
    pub number: usize,
    /// The command as written, whitespace-trimmed. Not parsed further here;
    /// see the module documentation for why.
    pub command: String,
}

/// One parsed function file: its commands, each with its source line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FunctionFile {
    /// In file order.
    pub lines: Vec<FunctionLine>,
}

impl FunctionFile {
    /// Read a function file under the rules in the module documentation.
    ///
    /// Never fails outright: a finding is produced for encoding problems and
    /// the rest of the design keeps the door open for per-line findings, but
    /// a well-encoded file always parses, because there is nothing at this
    /// layer a line of text can be wrong *about*.
    pub fn parse(bytes: &[u8], pack: &str, file: &str) -> (Self, Vec<Finding>) {
        let mut findings = Vec::new();
        let text = match std::str::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => {
                findings.push(Finding::error(
                    pack,
                    file,
                    format!(
                        "is not UTF-8: byte {} is not valid. Function files must \
                         be UTF-8, like the rest of a pack.",
                        error.valid_up_to()
                    ),
                ));
                return (Self::default(), findings);
            }
        };

        // Invisible to every editor that writes one, fatal to a parser that
        // does not: the mark would make the first command start with U+FEFF
        // and never match anything again.
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);

        let mut lines = Vec::new();
        for (index, raw) in text.split('\n').enumerate() {
            let line = raw.strip_suffix('\r').unwrap_or(raw).trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            lines.push(FunctionLine {
                number: index + 1,
                command: line.to_owned(),
            });
        }

        (Self { lines }, findings)
    }

    /// How many commands the file runs. Comments and blanks are not commands
    /// and are not counted; this is the number a `/reload` summary wants.
    pub fn command_count(&self) -> usize {
        self.lines.len()
    }
}

/// One loaded function, and where it came from — the function-side twin of
/// [`crate::Resource`], minus the JSON value it has no use for.
#[derive(Debug, Clone)]
pub struct LoadedFunction {
    pub file: FunctionFile,
    /// The pack that won.
    pub pack: String,
    /// The file inside that pack.
    pub path: String,
    /// Packs that defined this function and were overridden, earliest first.
    pub overridden: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> (FunctionFile, Vec<Finding>) {
        FunctionFile::parse(text.as_bytes(), "p", "f.mcfunction")
    }

    #[test]
    fn commands_keep_their_commands_and_lose_nothing_else() {
        let (file, findings) = parse("say one\n\n# a comment\ntellraw @a \"two\"\n");
        assert!(findings.is_empty(), "{findings:?}");
        assert_eq!(file.command_count(), 2);
        assert_eq!(file.lines[0].command, "say one");
        assert_eq!(file.lines[1].command, r#"tellraw @a "two""#);
    }

    #[test]
    fn a_comment_is_a_comment_even_after_indentation_and_blank_lines_are_silent() {
        // Vanilla trims first and checks for `#` second, so an indented
        // comment is still a comment. A loader that split those rules across
        // two steps in the other order would run the comment as a command.
        let (file, findings) = parse("   # indented note\n\t\ntabbed\tcommand  \n");
        assert!(findings.is_empty(), "{findings:?}");
        assert_eq!(file.command_count(), 1);
        assert_eq!(file.lines[0].command, "tabbed\tcommand");
        assert_eq!(
            file.lines[0].number, 3,
            "the number counts physical lines, blanks included"
        );
    }

    #[test]
    fn windows_line_endings_split_cleanly() {
        let (file, findings) = parse("say a\r\nsay b\r\n");
        assert!(findings.is_empty(), "{findings:?}");
        assert_eq!(file.command_count(), 2);
        assert_eq!(
            file.lines[1].command, "say b",
            "no carriage return survives"
        );
    }

    #[test]
    fn an_empty_file_and_a_comments_only_file_are_both_empty_functions() {
        let (empty, findings) = parse("");
        assert!(findings.is_empty() && empty.lines.is_empty());
        let (noted, findings) = parse("# nothing but notes\n\n# more notes\n");
        assert!(findings.is_empty() && noted.lines.is_empty());
    }

    #[test]
    fn invalid_utf8_is_an_error_naming_the_byte() {
        let (file, findings) = FunctionFile::parse(&[b's', b'a', b'y', b' ', 0xff], "p", "f");
        assert_eq!(file, FunctionFile::default());
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("UTF-8"), "{}", findings[0]);
        assert!(findings[0].message.contains('4'), "{}", findings[0]);
    }

    #[test]
    fn a_byte_order_mark_does_not_become_the_first_command() {
        let mut bytes = "\u{feff}".as_bytes().to_vec();
        bytes.extend_from_slice(b"say hi");
        let (file, findings) = FunctionFile::parse(&bytes, "p", "f");
        assert!(findings.is_empty(), "{findings:?}");
        assert_eq!(file.lines[0].command, "say hi");
    }

    #[test]
    fn line_numbers_survive_everything_skipped_before_them() {
        let (file, _) = parse("\n\n# c\n\nsay x\n");
        assert_eq!(file.lines[0].number, 5);
    }
}

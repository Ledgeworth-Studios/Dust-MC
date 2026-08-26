//! Hostile inputs: what happens when the bytes want to hurt someone.
//!
//! # The threat model this file works through
//!
//! Every reader in this crate is reachable from data an attacker chose: packet
//! NBT arrives from the client, and world files arrive from whoever edited
//! them. Three defences protect the process — the depth limit guards the
//! *stack*, the length checks guard against headers that lie about what the
//! input contains, and the heap budget guards against documents that are
//! honest about their size but amplify enormously once decoded. Each defence
//! gets a test that tries to defeat it. The last section is different: a few
//! thousand mutated encodings fed through the readers with only one rule,
//! return an answer, never die — and where the `exact` entry point accepts a
//! document, rewriting it must give back exactly the mutated bytes, because
//! acceptance means the input was already canonical.
//!
//! All pseudo-randomness here comes from a fixed-seed generator written out
//! below, so the run is reproducible: a failure names its seed and iteration
//! and can be replayed by changing two constants.

mod support;

use dust_nbt::{read, snbt, Error, Limits, Mode, Tag};

use support::{corpus_documents, corpus_texts, mutate, SplitMix64};

// ---------------------------------------------------------------------------
// Building pathological inputs, iteratively
//
// Nothing here may recurse while constructing its input. The whole point is
// that the *reader* meets the depth; a test that built the document
// recursively would be betting the test's stack against the reader's.
// ---------------------------------------------------------------------------

/// A file-form root holding `depth` nested containers, written out linearly.
///
/// Both builders spell the layout forwards rather than wrapping values around
/// each other: a wrapper level is five bytes (`TAG_List`/`TAG_Compound`, a
/// name or a length), and the whole document is assembled in one pass, so a
/// half-million-deep input costs kilobytes of copying rather than seconds.
///
/// Layout for lists, `depth` levels ending at `TAG_Byte(42)`:
/// `[09 00 00]` then `(09 00 00 00 01)` per wrapper — element type list,
/// length one — then `(01 00 00 00 01 2a)` for the innermost list of one
/// byte. Compounds mirror it with field headers `0a 00 01 6e` ("n") and one
/// terminating zero byte per compound.
fn nested_containers(kind: u8, depth: usize) -> Vec<u8> {
    assert!(depth >= 1);
    let mut out = Vec::with_capacity(3 + depth * 9);
    out.extend_from_slice(&[kind, 0x00, 0x00]);

    match kind {
        // Lists nest through element type + length; the innermost carries the
        // byte directly.
        0x09 => {
            for _ in 0..depth - 1 {
                out.extend_from_slice(&[0x09, 0x00, 0x00, 0x00, 0x01]);
            }
            out.extend_from_slice(&[0x01, 0x00, 0x00, 0x00, 0x01, 0x2a]);
        }
        // Compounds nest through field headers; each level closes itself.
        0x0a => {
            for _ in 0..depth - 1 {
                out.extend_from_slice(&[0x0a, 0x00, 0x01, 0x6e]);
            }
            out.extend_from_slice(&[0x01, 0x00, 0x01, 0x6e, 0x2a]);
            out.extend(std::iter::repeat_n(0u8, depth));
        }
        other => panic!("no such container kind {other:#x}"),
    }
    out
}

fn nested_compounds(depth: usize) -> Vec<u8> {
    nested_containers(0x0a, depth)
}

fn nested_lists(depth: usize) -> Vec<u8> {
    nested_containers(0x09, depth)
}

/// SNBT spelling of `depth` nested lists around a lone integer.
fn nested_snbt_lists(depth: usize) -> String {
    let mut out = String::new();
    out.push_str(&"[".repeat(depth));
    out.push('0');
    out.push_str(&"]".repeat(depth));
    out
}

/// SNBT spelling of `depth` nested compounds around a lone integer.
fn nested_snbt_compounds(depth: usize) -> String {
    let mut out = String::new();
    out.push_str(&"{a:".repeat(depth));
    out.push('0');
    out.push_str(&"}".repeat(depth));
    out
}

// ---------------------------------------------------------------------------
// The depth limit, on both readers
// ---------------------------------------------------------------------------

const DEFAULT_LIMIT: usize = 512;

/// Vanilla's own limit, and the reason it matches: past it, a document a real
/// client cannot send anyway is refused by comparison rather than met with
/// stack. Reading *at* the limit runs where legal documents live — see
/// [`support::on_a_large_stack`] — while every refusal here stays shallow
/// enough for any thread, which is rather the point of a limit.
#[test]
fn the_binary_reader_refuses_one_level_past_the_limit() {
    // Every assertion here drives the reader to its full allowance before the
    // limit fires, which in an unoptimised build is more frames than a
    // default test-thread stack guarantees — see
    // [`support::on_a_large_stack`] for why that is the thread's problem and
    // not a reason to shrink the document.
    support::on_a_large_stack(|| {
        let legal = nested_lists(DEFAULT_LIMIT);
        assert!(
            read::from_bytes(&legal).is_ok(),
            "exactly-at-the-limit is a legitimate document and must parse"
        );
        let legal = nested_compounds(DEFAULT_LIMIT);
        assert!(read::from_bytes(&legal).is_ok());

        let illegal = nested_compounds(DEFAULT_LIMIT + 1);
        match read::from_bytes(&illegal).expect_err("one past the limit") {
            Error::TooDeep { limit, .. } => assert_eq!(limit, DEFAULT_LIMIT),
            other => panic!("expected TooDeep, got {other:?}"),
        }

        // Lists nest through the same counter, and the limit does not care
        // which container did the nesting.
        let illegal = nested_lists(DEFAULT_LIMIT + 1);
        assert!(matches!(
            read::from_bytes(&illegal),
            Err(Error::TooDeep { .. })
        ));
    });
}

/// The refusal must happen *before* the stack is consumed: a document far past
/// the limit is turned away after `limit + 1` frames, not after all of them —
/// a hundred thousand levels of nesting cost the same five hundred frames of
/// stack as five hundred.
#[test]
fn absurd_depth_is_refused_long_before_it_can_exhaust_a_stack() {
    support::on_a_large_stack(|| {
        let absurd = nested_compounds(100_000);
        assert!(matches!(
            read::from_bytes(&absurd),
            Err(Error::TooDeep { .. })
        ));

        let absurd = nested_lists(100_000);
        assert!(matches!(
            read::from_bytes(&absurd),
            Err(Error::TooDeep { .. })
        ));
    });
}

#[test]
fn the_snbt_parser_refuses_one_level_past_the_limit() {
    assert!(snbt::parse(&nested_snbt_lists(DEFAULT_LIMIT)).is_ok());
    assert!(snbt::parse(&nested_snbt_compounds(DEFAULT_LIMIT)).is_ok());
    assert!(
        snbt::parse(&nested_snbt_lists(DEFAULT_LIMIT + 1)).is_err(),
        "the textual parser answers to the same limit as the binary one"
    );
    assert!(snbt::parse(&nested_snbt_compounds(DEFAULT_LIMIT + 1)).is_err());
    assert!(snbt::parse(&nested_snbt_compounds(500_000)).is_err());
}

/// The depth knob is real: raised deliberately, on a thread given the stack
/// to afford it, a deeper-than-vanilla document parses correctly. This is the
/// flip side of [`support::on_a_large_stack`] — the limit is configuration,
/// and this proves the configuration is honoured rather than decorative.
#[test]
fn a_raised_limit_on_a_generous_stack_reads_deeper_documents() {
    support::on_a_large_stack(|| {
        // Eight times vanilla's allowance. Deeper than this and an
        // unoptimised build's frame cost starts to crowd even sixty-four
        // megabytes, which is itself the honest measurement: the limit exists
        // because real stacks are finite, and "how deep can we go" has an
        // answer in kilobytes, not in principles.
        const DEPTH: usize = 4_000;
        let deep = nested_lists(DEPTH);

        let limits = Limits {
            max_depth: DEPTH,
            ..Limits::FILE
        };
        let mut reader = read::Reader::new(&deep, limits);
        let document = reader
            .read_root(Mode::File)
            .expect("reads within its allowance");
        assert_eq!(document.name, "");
        assert!(
            document.tag.as_list().is_some(),
            "the outermost value is the list"
        );

        // And the same knob tightens as well as loosens.
        let strict = Limits {
            max_depth: 8,
            ..Limits::FILE
        };
        assert!(matches!(
            read::from_bytes_with(&deep, strict),
            Err(Error::TooDeep { limit: 8, .. })
        ));
    });
}

// ---------------------------------------------------------------------------
// The heap budget: honest input that still costs too much
// ---------------------------------------------------------------------------

/// An empty compound costs one byte of input and one `Tag` of memory: a
/// forty-fold amplifier if the width of `Tag` is anything like thirty bytes.
/// This is the case the length checks have no reason to object to — every
/// byte of the input is legitimate — and the reason the budget exists.
#[test]
fn a_honest_document_that_amplifies_hits_the_heap_budget() {
    // A network-form list of 100,000 empty compounds: about 100 KB of input.
    let mut document = vec![0x09, 0x0a, 0x00, 0x01, 0x86, 0xa0];
    document.extend(std::iter::repeat_n(0u8, 100_000));

    match dust_nbt::read::from_bytes_network(&document) {
        Err(Error::HeapBudgetExceeded { limit, .. }) => {
            assert_eq!(limit, Limits::NETWORK.max_heap_bytes);
        }
        other => panic!("expected HeapBudgetExceeded, got {other:?}"),
    }

    // The same document within a generous budget reads fine, so the budget —
    // not the shape — is what refused it.
    let generous = Limits {
        max_heap_bytes: usize::MAX,
        max_depth: 512,
    };
    assert!(dust_nbt::read::from_bytes_network_with(&document, generous).is_ok());
}

/// Strings are charged too, at their encoded length: a single `TAG_String`
/// cannot exceed the `u16` prefix, but a document of many strings adds up,
/// and the budget counts every one.
#[test]
fn strings_charge_the_budget_and_a_small_budget_is_enforced() {
    let mut document = write_string_document("x", 300);

    let tight = Limits {
        max_heap_bytes: 256,
        max_depth: 512,
    };
    assert!(matches!(
        dust_nbt::read::from_bytes_with(&document, tight),
        Err(Error::HeapBudgetExceeded { .. })
    ));

    let roomy = Limits {
        max_heap_bytes: 4 * 1024 * 1024,
        max_depth: 512,
    };
    assert!(dust_nbt::read::from_bytes_with(&document, roomy).is_ok());

    // Doubling the content doubles the charge; the budget tracks reality
    // rather than tripping at some magic constant.
    document = write_string_document("x", 600);
    assert!(matches!(
        dust_nbt::read::from_bytes_with(&document, tight),
        Err(Error::HeapBudgetExceeded { .. })
    ));
}

/// A file-form document whose root is a compound holding one string of `n`
/// characters.
fn write_string_document(text_char: &str, n: usize) -> Vec<u8> {
    let tag = Tag::String(std::iter::repeat_n(text_char, n).collect());
    dust_nbt::write::to_vec("", &tag).expect("writes")
}

// ---------------------------------------------------------------------------
// The mutation loop
// ---------------------------------------------------------------------------

/// A few thousand mutated encodings: every one gets an answer, none kills the
/// process, and everything the exact reader accepts rewrites to itself.
#[test]
fn thousands_of_mutated_encodings_never_panic_and_accepted_ones_rewrite_identically() {
    const ITERATIONS: usize = 4_000;
    const SEED: u64 = 0x5EED_0001;

    let originals = corpus_documents();
    let mut rng = SplitMix64::new(SEED);
    let mut accepted = 0usize;
    let mut rejected = 0usize;

    for iteration in 0..ITERATIONS {
        let mut bytes = originals[rng.below(originals.len())].clone();
        for _ in 0..1 + rng.below(8) {
            mutate(&mut rng, &mut bytes);
        }

        // File mode, exact: an answer either way, and acceptance carries the
        // canonicality invariant with it.
        match read::from_bytes_exact(&bytes) {
            Ok(document) => {
                accepted += 1;
                let rewritten =
                    dust_nbt::write::to_vec(&document.name, &document.tag).expect("writes");
                assert_eq!(
                    rewritten, bytes,
                    "iteration {iteration} (seed {SEED}): accepted a document that does \
                     not rewrite to itself"
                );
            }
            Err(_) => rejected += 1,
        }

        // Network mode over the same bytes: an answer, either kind.
        let _ = dust_nbt::read::from_bytes_network_with(&bytes, Limits::FILE);
    }

    assert!(
        accepted > 50 && rejected > 50,
        "the mutation rate should leave both outcomes common, got {accepted} accepted \
         and {rejected} rejected; a run of nearly-all-one-kind is testing nothing"
    );
}

/// The same treatment for the SNBT parser, over text mutations.
#[test]
fn thousands_of_mutated_snbt_texts_never_panic() {
    const ITERATIONS: usize = 4_000;
    const SEED: u64 = 0x5EED_0002;

    let originals = corpus_texts();
    let mut rng = SplitMix64::new(SEED);
    let mut parsed = 0usize;
    let mut refused = 0usize;
    let mut skipped = 0usize;

    for iteration in 0..ITERATIONS {
        let mut bytes = originals[rng.below(originals.len())].clone().into_bytes();
        for _ in 0..1 + rng.below(8) {
            mutate(&mut rng, &mut bytes);
        }
        // Mutating raw bytes can tear a multi-byte character; the parser takes
        // `&str`, so a torn input is skipped and counted rather than forced
        // through a lossy repair that would hide the tear.
        let Ok(text) = String::from_utf8(bytes) else {
            skipped += 1;
            continue;
        };

        match snbt::parse(&text) {
            Ok(_) => parsed += 1,
            Err(error) => {
                refused += 1;
                assert!(
                    error.offset <= text.len(),
                    "iteration {iteration} (seed {SEED}): error at {} beyond a {} byte \
                     input",
                    error.offset,
                    text.len()
                );
            }
        }
    }

    assert!(
        parsed + refused > ITERATIONS / 2,
        "too many inputs were skipped as non-UTF-8 ({skipped} of {ITERATIONS}); the \
         mutation rate needs tuning"
    );
    assert!(
        parsed > 50 && refused > 50,
        "both outcomes should be common"
    );
}

/// Empty and one-byte inputs, the boundary every reader hits first.
#[test]
fn nothing_at_all_is_an_error_not_a_crash() {
    assert!(matches!(
        read::from_bytes(&[]),
        Err(Error::UnexpectedEnd {
            while_reading: "a tag id",
            ..
        })
    ));
    assert!(read::from_bytes(&[0x0a]).is_err());
    assert!(read::from_bytes(&[0xff]).is_err());
    assert!(read::from_bytes_network(&[0x0a]).is_err());
    assert!(snbt::parse("").is_err());
    assert!(snbt::parse("{").is_err());
    assert!(snbt::parse("[").is_err());
}

// ---------------------------------------------------------------------------
// Targeted mutations
//
// The blind loop above edits anywhere, so most of its hits land in payloads
// where nothing interesting happens. These classes aim at the structure: the
// bytes that *direct* a parse — length prefixes, tag ids, element types —
// because that is where a hostile encoder aims too. Each document is laid out
// by hand here rather than produced by the writer, so the offsets of those
// bytes are known exactly and the mutations can hit them on every iteration
// instead of by luck.
//
// The invariant is the one the blind loop checks, plus the borrowed reader:
// an answer either way, never a panic, and anything `exact` accepts rewrites
// to itself and agrees with the owned reader's tree.
// ---------------------------------------------------------------------------

/// What lives at a recorded offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Prefix {
    /// An `i32` array or list length; lies may go up or down.
    Word32,
    /// A `u16` string length prefix.
    Word16,
    /// A tag id or list element-type byte.
    TagId,
}

/// A document with its structural offsets recorded.
struct Laid {
    bytes: Vec<u8>,
    marks: Vec<(usize, Prefix)>,
}

impl Laid {
    fn push(&mut self, byte: u8, mark: Option<Prefix>) -> usize {
        self.bytes.push(byte);
        let at = self.bytes.len() - 1;
        if let Some(prefix) = mark {
            self.marks.push((at, prefix));
        }
        at
    }

    fn extend(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    /// A compound root carrying one of each interesting shape: a double list,
    /// a byte array, a string, and a nested empty-typed list. Every header
    /// byte is recorded as it is written.
    fn specimen() -> Self {
        let mut laid = Laid {
            bytes: Vec::with_capacity(128),
            marks: Vec::new(),
        };

        // Root compound, empty name.
        laid.push(0x0a, Some(Prefix::TagId));
        laid.extend(&[0x00, 0x00]);

        // Field "pos": a list of three doubles.
        laid.push(0x09, Some(Prefix::TagId));
        laid.extend(&[0x00, 0x03, b'p', b'o', b's']);
        laid.push(0x06, Some(Prefix::TagId)); // element type: TAG_Double
        let list_len_at = laid.bytes.len();
        laid.extend(&3i32.to_be_bytes());
        laid.marks.push((list_len_at, Prefix::Word32));
        laid.extend(&1.5f64.to_be_bytes());
        laid.extend(&64.0f64.to_be_bytes());
        laid.extend(&(-2.25f64).to_be_bytes());

        // Field "raw": a byte array of five.
        laid.push(0x07, Some(Prefix::TagId));
        laid.extend(&[0x00, 0x03, b'r', b'a', b'w']);
        let raw_len_at = laid.bytes.len();
        laid.extend(&5i32.to_be_bytes());
        laid.marks.push((raw_len_at, Prefix::Word32));
        laid.extend(&[1, 2, 3, 4, 5]);

        // Field "name": a string of nine.
        laid.push(0x08, Some(Prefix::TagId));
        laid.extend(&[0x00, 0x04, b'n', b'a', b'm', b'e']);
        let name_len_at = laid.bytes.len();
        laid.extend(&9u16.to_be_bytes());
        laid.marks.push((name_len_at, Prefix::Word16));
        laid.extend(b"structure");

        // Field "spare": an empty list vanilla-flavoured (element type End).
        laid.push(0x09, Some(Prefix::TagId));
        laid.extend(&[0x00, 0x05, b's', b'p', b'a', b'r', b'e']);
        laid.push(0x00, Some(Prefix::TagId));
        let spare_len_at = laid.bytes.len();
        laid.extend(&0i32.to_be_bytes());
        laid.marks.push((spare_len_at, Prefix::Word32));

        // End of root.
        laid.push(0x00, None);
        laid
    }

    fn word32_marks(&self) -> Vec<usize> {
        self.marks
            .iter()
            .filter(|(_, prefix)| *prefix == Prefix::Word32)
            .map(|(at, _)| *at)
            .collect()
    }
}

/// Flip a bit within four bytes of a recorded length prefix or tag id — close
/// enough to corrupt what the byte sits next to, wherever it lands.
fn flip_bit_near(rng: &mut SplitMix64, document: &mut Laid) {
    let (at, _) = document.marks[rng.below(document.marks.len())];
    let window = 4;
    let low = at.saturating_sub(window);
    let high = (at + window).min(document.bytes.len() - 1);
    let target = low + rng.below(high - low + 1);
    document.bytes[target] ^= 1 << rng.below(8);
}

/// Rewrite a recorded length prefix to a lie. Upward lies claim more than the
/// input holds; downward ones claim less, leaving trailing bytes a lazy
/// reader might accept as padding — both are real encoders' mistakes.
fn lie_about_length(rng: &mut SplitMix64, document: &mut Laid) {
    let marks = document.word32_marks();
    let at = marks[rng.below(marks.len())];
    let original = i32::from_be_bytes([
        document.bytes[at],
        document.bytes[at + 1],
        document.bytes[at + 2],
        document.bytes[at + 3],
    ]);
    let lie = match rng.below(8) {
        // Upward: more elements than bytes exist.
        0 => original.wrapping_mul(2),
        1 => original.saturating_add(1_000),
        2 => i32::MAX,
        // Downward: fewer claimed than were written.
        3 => original.saturating_sub(1),
        4 => original / 2,
        5 => 0,
        // And the sign bit, which no length may carry.
        6 => -1,
        _ => i32::MIN,
    };
    document.bytes[at..at + 4].copy_from_slice(&lie.to_be_bytes());

    // Sometimes the string prefix instead, which is unsigned but small.
    if rng.below(2) == 0 {
        let &(at, Prefix::Word16) = document
            .marks
            .iter()
            .find(|(_, prefix)| *prefix == Prefix::Word16)
            .expect("the specimen carries a string")
        else {
            unreachable!()
        };
        let lie16 = match rng.below(3) {
            0 => u16::MAX,
            1 => 0u16,
            _ => rng.below(u16::MAX as usize + 1) as u16,
        };
        document.bytes[at..at + 2].copy_from_slice(&lie16.to_be_bytes());
    }
}

/// Overwrite a recorded tag id with one of the rare array types — legal ids
/// that appear rarely in real documents, so parsers meet them least.
fn swap_to_rare_tag(rng: &mut SplitMix64, document: &mut Laid) {
    let tag_marks: Vec<usize> = document
        .marks
        .iter()
        .filter(|(_, prefix)| *prefix == Prefix::TagId)
        .map(|(at, _)| *at)
        .collect();
    let at = tag_marks[rng.below(tag_marks.len())];
    let rare = [0x07, 0x0b, 0x0c][rng.below(3)];
    document.bytes[at] = rare;
}

/// Splice a byte-array header plus payload into the middle of the double
/// list's payload: nested-container confusion, asking the list reader to make
/// sense of another tag's anatomy mid-body.
fn splice_array_into_list(rng: &mut SplitMix64, document: &mut Laid) {
    // The payload region of "pos": its length prefix is the Word32 whose
    // preceding byte is the list's TAG_Double element type.
    let list_len_at = document
        .marks
        .iter()
        .filter_map(|(at, prefix)| {
            (*prefix == Prefix::Word32 && *at >= 1 && document.bytes[*at - 1] == 0x06)
                .then_some(*at)
        })
        .next()
        .expect("the specimen carries a typed list");
    let insert_at = list_len_at + 4 + rng.below(24).min(document.bytes.len() - list_len_at - 4);

    let mut fragment = vec![0x07u8];
    fragment.extend_from_slice(&5i32.to_be_bytes());
    fragment.extend_from_slice(&[9u8; 5]);
    for (index, byte) in fragment.into_iter().enumerate() {
        document.bytes.insert(insert_at + index, byte);
    }
}

/// Shared body of the targeted loops: answer-only, canonical-if-accepted, and
/// in agreement with the owned reader when accepted.
fn run_targeted(
    iterations: usize,
    seed: u64,
    label: &str,
    build: impl Fn() -> Laid,
    mutate_one: impl Fn(&mut SplitMix64, &mut Laid),
) {
    let mut rng = SplitMix64::new(seed);
    let mut accepted = 0usize;
    let mut refused = 0usize;

    for iteration in 0..iterations {
        let mut document = build();
        mutate_one(&mut rng, &mut document);

        match read::from_bytes_exact(&document.bytes) {
            Ok(parsed) => {
                accepted += 1;
                let rewritten = dust_nbt::write::to_vec(&parsed.name, &parsed.tag).expect("writes");
                assert_eq!(
                    rewritten, document.bytes,
                    "{label} iteration {iteration} (seed {seed}): accepted a document \
                     that does not rewrite to itself"
                );
                let borrowed = dust_nbt::borrow::from_bytes_with(&document.bytes, Limits::FILE)
                    .expect("the owned reader accepted, so must the borrowed one");
                let rebuilt = materialise_borrowed(&borrowed, borrowed.root());
                assert_eq!(
                    rebuilt, parsed.tag,
                    "{label} iteration {iteration}: readers disagree"
                );
            }
            Err(_) => refused += 1,
        }

        // Network mode over the same bytes: an answer of some kind.
        let _ = read::from_bytes_network_with(&document.bytes, Limits::FILE);
        let _ = dust_nbt::borrow::from_bytes_network_with(&document.bytes, Limits::FILE);
    }

    assert!(
        accepted > 10 || refused > iterations / 2,
        "{label}: neither outcome dominated as designed ({accepted} accepted)"
    );
}

/// Rebuild an owned tag from a borrowed view, locally: the same walk
/// `tests/borrow.rs` uses, kept here so this file reads without cross-suite
/// imports beyond the shared support module.
fn materialise_borrowed(
    document: &dust_nbt::borrow::Document<'_>,
    value: &dust_nbt::borrow::Value<'_>,
) -> Tag {
    use dust_nbt::borrow::Value;
    match value {
        Value::Byte(v) => Tag::Byte(*v),
        Value::Short(v) => Tag::Short(*v),
        Value::Int(v) => Tag::Int(*v),
        Value::Long(v) => Tag::Long(*v),
        Value::Float(v) => Tag::Float(*v),
        Value::Double(v) => Tag::Double(*v),
        Value::ByteArray(bytes) => Tag::ByteArray(bytes.iter().map(|b| b as i8).collect()),
        Value::String(text) => Tag::String(document.text(*text).to_owned()),
        Value::List(list) => {
            let elements = match list.values() {
                Some(values) => values
                    .iter()
                    .map(|value| materialise_borrowed(document, value))
                    .collect(),
                None => (0..list.len())
                    .map(|index| {
                        list.get(index)
                            .map(|value| materialise_borrowed(document, &value))
                            .expect("len bounds get")
                    })
                    .collect(),
            };
            Tag::List(
                dust_nbt::List::from_elements(list.element_type(), elements)
                    .expect("homogeneous on parse"),
            )
        }
        Value::Compound(compound) => {
            let mut out = dust_nbt::Compound::new();
            for (name, value) in compound.iter() {
                out.append(
                    document.text(*name).to_owned(),
                    materialise_borrowed(document, value),
                );
            }
            Tag::Compound(out)
        }
        Value::IntArray(ints) => Tag::IntArray(ints.iter().collect()),
        Value::LongArray(longs) => Tag::LongArray(longs.iter().collect()),
    }
}

#[test]
fn bit_flips_next_to_length_prefixes_stay_answer_only() {
    const ITERATIONS: usize = 2_000;
    const SEED: u64 = 0x5EED_0010;
    run_targeted(
        ITERATIONS,
        SEED,
        "bit-flip-near-prefix",
        Laid::specimen,
        |rng, document| flip_bit_near(rng, document),
    );
}

#[test]
fn length_prefixes_that_lie_upward_and_downward_are_answered_safely() {
    const ITERATIONS: usize = 2_000;
    const SEED: u64 = 0x5EED_0011;
    run_targeted(
        ITERATIONS,
        SEED,
        "length-lie",
        Laid::specimen,
        |rng, document| lie_about_length(rng, document),
    );
}

#[test]
fn tag_ids_swapped_to_the_rare_array_types_stay_answer_only() {
    const ITERATIONS: usize = 2_000;
    const SEED: u64 = 0x5EED_0012;
    run_targeted(
        ITERATIONS,
        SEED,
        "rare-tag-swap",
        Laid::specimen,
        |rng, document| swap_to_rare_tag(rng, document),
    );
}

#[test]
fn an_array_header_spliced_into_a_list_is_answered_not_fatal() {
    const ITERATIONS: usize = 2_000;
    const SEED: u64 = 0x5EED_0013;
    run_targeted(
        ITERATIONS,
        SEED,
        "array-splice",
        Laid::specimen,
        |rng, document| splice_array_into_list(rng, document),
    );
}

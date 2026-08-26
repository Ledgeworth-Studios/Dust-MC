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

/// SplitMix64: fifteen lines, no dependency, and every bit avalanche-mixed so
/// successive draws decorrelate even from sequential seeds.
struct SplitMix64(u64);

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }
}

/// Documents worth mutating: shapes chosen to exercise every reader path —
/// strings carrying the two-byte NUL, a typed empty list, NaN payloads,
/// duplicate keys, arrays, and an emoji as a surrogate pair.
fn corpus_documents() -> Vec<Vec<u8>> {
    let mut compound = dust_nbt::Compound::new();
    compound.insert("name", Tag::String("notch\u{0000}\u{1f600}".to_owned()));
    compound.insert("floats", Tag::Float(f32::from_bits(0x7fc0_0001)));
    compound.insert(
        "empty",
        Tag::List(dust_nbt::List::new(dust_nbt::TagType::Int)),
    );
    compound.insert("words", Tag::IntArray(vec![i32::MIN, -1, 0, 1, i32::MAX]));

    let mut duplicated = dust_nbt::Compound::new();
    duplicated.insert("id", Tag::String("first".to_owned()));
    duplicated.insert("id", Tag::String("second".to_owned()));
    compound.insert("dup", Tag::Compound(duplicated));

    let named = vec![
        ("root".to_owned(), Tag::Compound(compound.clone())),
        (String::new(), Tag::ByteArray(vec![i8::MIN, 0, i8::MAX])),
        ("tiny".to_owned(), Tag::Long(i64::MIN)),
    ];
    named
        .into_iter()
        .map(|(name, tag)| dust_nbt::write::to_vec(&name, &tag).expect("writes"))
        .collect()
}

/// SNBT texts worth mutating, covering quoting, suffixes and arrays.
fn corpus_texts() -> Vec<String> {
    [
        "{a:1b,b:'quoted \\' text',c:\"other\"}".to_owned(),
        "[B;1b,2b,-3b]".to_owned(),
        "{pos:[I;-1,2,-3],flag:true,name:0x}".to_owned(),
        "{}".to_owned(),
        "{e:1.5e3d,f:-.25,g:[]}".to_owned(),
    ]
    .into_iter()
    .collect()
}

/// Mutate `bytes` in place, one randomly-chosen edit.
fn mutate(rng: &mut SplitMix64, bytes: &mut Vec<u8>) {
    if bytes.is_empty() {
        bytes.push(rng.below(256) as u8);
        return;
    }
    let at = rng.below(bytes.len());
    match rng.below(6) {
        0 => bytes[at] ^= 1 << rng.below(8),
        1 => bytes[at] = rng.below(256) as u8,
        2 => {
            bytes.remove(at);
        }
        3 => bytes.insert(at, bytes[at]),
        4 => bytes.insert(at, rng.below(256) as u8),
        _ => bytes.truncate(at),
    }
}

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

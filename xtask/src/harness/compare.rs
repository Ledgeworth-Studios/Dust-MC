//! `harness compare` — what changed between two captures.
//!
//! The whole point of differential testing is a diff somebody can act on, so
//! the output is rows, not prose: one row per chunk that is missing, extra or
//! divergent, with both digests side by side for the divergent ones. Exit
//! codes carry the verdict so CI can use this without reading it:
//!
//! - `0` — identical. A successful run of the tool reporting no finding.
//! - `1` — differences found. Also a successful run; the *finding* is the
//!   result. This is deliberately not an error.
//! - `2` — the comparison could not run at all: unreadable files, mismatched
//!   seeds or data versions. Distinct from 1 so "the worlds differ" and "the
//!   inputs were nonsense" never share a code.
//!
//! Sets from different seeds or different vanilla data versions are refused
//! rather than reported as "everything diverged": block names themselves move
//! between versions, so such a diff is noise wearing the shape of signal.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::digest::{self, ChunkDigest, DigestSet, DIGEST_LEN};

/// Exit code: the two captures differ.
pub const EXIT_DIFFERENT: u8 = 1;
/// Exit code: the comparison itself failed (bad input, not a difference).
pub const EXIT_UNRUNNABLE: u8 = 2;

#[derive(Debug)]
pub struct Options {
    /// Path to a capture set: either a directory holding `chunks.bin` or the
    /// binary file itself.
    pub a: PathBuf,
    pub b: PathBuf,
    /// Where to also write the diff as TSV, if asked.
    pub tsv: Option<PathBuf>,
}

/// Parse the `harness compare` argument list.
pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut positional = Vec::new();
    let mut tsv = None;
    let mut seen: Vec<(&'static str, String)> = Vec::new();
    let mut at = 0;
    while at < args.len() {
        match args[at].as_str() {
            "--tsv" => {
                at = super::take_value(&mut seen, "--tsv", args, at + 1)?;
                tsv = Some(PathBuf::from(seen.last().expect("just stored").1.clone()));
            }
            other => {
                positional.push(other.to_owned());
                at += 1;
            }
        }
    }
    let [a, b] = positional.as_slice() else {
        return Err(format!(
            "compare needs exactly two capture sets\n\n{}",
            super::USAGE
        ));
    };
    Ok(Options {
        a: PathBuf::from(a),
        b: PathBuf::from(b),
        tsv,
    })
}

/// Compare two digest sets, print the report, and hand back the exit code.
///
/// Operational failures (unreadable files, mismatched context) are reported
/// here on stderr and answered with [`EXIT_UNRUNNABLE`] rather than handed up
/// as errors: this verb's whole contract is that its exit codes mean the same
/// thing everywhere, so a script can trust 1 without reading stderr.
pub fn run(options: &Options) -> ExitCode {
    let outcome = (|| -> Result<bool, String> {
        let a = read_set(&options.a)?;
        let b = read_set(&options.b)?;
        refuse_mismatched_context(&a, &options.a, &b, &options.b)?;

        let report = diff(&a, &b);
        print_report(&a, &b, &report);

        if let Some(path) = &options.tsv {
            std::fs::write(path, render_tsv(&report))
                .map_err(|e| format!("could not write {}: {e}", path.display()))?;
            println!("wrote {}", path.display());
        }
        Ok(report.is_identical())
    })();

    match outcome {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(EXIT_DIFFERENT),
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::from(EXIT_UNRUNNABLE)
        }
    }
}

/// Accept a directory holding `chunks.bin` or the file itself.
fn read_set(path: &Path) -> Result<DigestSet, String> {
    let bin = if path.is_dir() {
        path.join("chunks.bin")
    } else {
        path.to_path_buf()
    };
    digest::read_bin(&bin).map_err(|e| format!("could not load {}: {e}", path.display()))
}

fn refuse_mismatched_context(
    a: &DigestSet,
    a_path: &Path,
    b: &DigestSet,
    b_path: &Path,
) -> Result<(), String> {
    if a.seed != b.seed {
        return Err(format!(
            "{} is seed {} but {} is seed {}; different seeds generate different worlds \
             by design, so there is nothing to diff",
            a_path.display(),
            a.seed,
            b_path.display(),
            b.seed
        ));
    }
    if a.data_version != b.data_version {
        return Err(format!(
            "{} is data version {} but {} is {}; block identities move between versions, \
             so cross-version digests are meaningless",
            a_path.display(),
            a.data_version,
            b_path.display(),
            b.data_version
        ));
    }
    Ok(())
}

/// One chunk's place in the diff, with its digests when it exists on both
/// sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    pub x: i32,
    pub z: i32,
    /// The `A`-side chunk, present exactly when [`DiffKind`] is divergent.
    pub a: Option<ChunkDigest>,
    pub b: Option<ChunkDigest>,
}

/// What kind of row a divergence is.
impl Divergence {
    pub fn kind(&self) -> &'static str {
        match (&self.a, &self.b) {
            (Some(_), Some(_)) => "divergent",
            (Some(_), None) => "missing",
            (None, Some(_)) => "extra",
            (None, None) => unreachable!("a diff row names at least one side"),
        }
    }

    /// Which parts of a two-sided chunk disagree, by name.
    ///
    /// Empty for missing/extra rows, where absence is the whole story.
    pub fn differing_parts(&self) -> Vec<&'static str> {
        let (Some(a), Some(b)) = (&self.a, &self.b) else {
            return Vec::new();
        };
        let mut parts = Vec::new();
        if a.blocks != b.blocks {
            parts.push("blocks");
        }
        if a.biomes != b.biomes {
            parts.push("biomes");
        }
        for name in heightmap_names(a).collect::<Vec<_>>() {
            let left = a.heightmaps.iter().find(|(n, _)| n.as_str() == name);
            let right = b.heightmaps.iter().find(|(n, _)| n.as_str() == name);
            if left.map(|(_, d)| d) != right.map(|(_, d)| d) {
                parts.push("heightmap");
                break;
            }
        }
        parts
    }
}

fn heightmap_names(chunk: &ChunkDigest) -> impl Iterator<Item = &str> {
    chunk.heightmaps.iter().map(|(name, _)| name.as_str())
}

/// Everything that differs between two sets, in coordinate order.
#[derive(Debug, PartialEq, Eq)]
pub struct DiffReport {
    pub rows: Vec<Divergence>,
}

impl DiffReport {
    pub fn is_identical(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn count_of_kind(&self, kind: &str) -> usize {
        self.rows.iter().filter(|r| r.kind() == kind).count()
    }
}

/// Walk both coordinate-sorted sets once, classifying as we go.
///
/// Both inputs arrive sorted (the writer guarantees it and the reader checks
/// it), so this is a merge rather than a search: linear in the record count,
/// which keeps compares of large radii instant.
pub fn diff(a: &DigestSet, b: &DigestSet) -> DiffReport {
    let mut rows = Vec::new();
    let mut ia = 0;
    let mut ib = 0;
    while ia < a.chunks.len() || ib < b.chunks.len() {
        let order = match (a.chunks.get(ia), b.chunks.get(ib)) {
            (Some(ca), Some(cb)) => (ca.x, ca.z).cmp(&(cb.x, cb.z)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => unreachable!("loop condition"),
        };
        match order {
            std::cmp::Ordering::Less => {
                let c = &a.chunks[ia];
                rows.push(Divergence {
                    x: c.x,
                    z: c.z,
                    a: Some(c.clone()),
                    b: None,
                });
                ia += 1;
            }
            std::cmp::Ordering::Greater => {
                let c = &b.chunks[ib];
                rows.push(Divergence {
                    x: c.x,
                    z: c.z,
                    a: None,
                    b: Some(c.clone()),
                });
                ib += 1;
            }
            std::cmp::Ordering::Equal => {
                let ca = &a.chunks[ia];
                let cb = &b.chunks[ib];
                if ca != cb {
                    rows.push(Divergence {
                        x: ca.x,
                        z: ca.z,
                        a: Some(ca.clone()),
                        b: Some(cb.clone()),
                    });
                }
                ia += 1;
                ib += 1;
            }
        }
    }
    DiffReport { rows }
}

fn hex(digest: &[u8; DIGEST_LEN]) -> String {
    digest::hex(digest)
}

/// The human-readable stdout report: header facts, then one line per row.
fn print_report(a: &DigestSet, b: &DigestSet, report: &DiffReport) {
    println!(
        "comparing seed {} data version {}: {} chunks vs {} chunks",
        a.seed,
        a.data_version,
        a.chunks.len(),
        b.chunks.len()
    );
    for row in &report.rows {
        match row.kind() {
            "missing" => println!("missing\t{}\t{}\tin A only", row.x, row.z),
            "extra" => println!("extra\t{}\t{}\tin B only", row.x, row.z),
            _ => println!(
                "divergent\t{}\t{}\t({})\tblocks {} != {}\tbiomes {} != {}",
                row.x,
                row.z,
                row.differing_parts().join(","),
                row.a
                    .as_ref()
                    .map(|c| hex(&c.blocks))
                    .expect("divergent has a"),
                row.b
                    .as_ref()
                    .map(|c| hex(&c.blocks))
                    .expect("divergent has b"),
                row.a
                    .as_ref()
                    .map(|c| hex(&c.biomes))
                    .expect("divergent has a"),
                row.b
                    .as_ref()
                    .map(|c| hex(&c.biomes))
                    .expect("divergent has b"),
            ),
        }
    }
    println!("{}", summary_line(report));
}

/// The totals sentence: identical chunks are implied by what is absent from
/// the counts, but stated so a clean run still says something.
pub fn summary_line(report: &DiffReport) -> String {
    if report.is_identical() {
        "identical".to_owned()
    } else {
        format!(
            "{} divergent, {} missing, {} extra of the compared chunks",
            report.count_of_kind("divergent"),
            report.count_of_kind("missing"),
            report.count_of_kind("extra"),
        )
    }
}

/// The same rows as machine-friendly TSV, for further plumbing.
pub fn render_tsv(report: &DiffReport) -> String {
    let mut out = String::from("# kind\tchunk_x\tchunk_z\tparts\ta_blocks\tb_blocks\n");
    for row in &report.rows {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            row.kind(),
            row.x,
            row.z,
            row.differing_parts().join("+"),
            row.a.as_ref().map(|c| hex(&c.blocks)).unwrap_or_default(),
            row.b.as_ref().map(|c| hex(&c.blocks)).unwrap_or_default(),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(x: i32, z: i32, blocks: u8, biomes: u8) -> ChunkDigest {
        ChunkDigest {
            x,
            z,
            status: "full".to_owned(),
            blocks: [blocks; DIGEST_LEN],
            biomes: [biomes; DIGEST_LEN],
            heightmaps: vec![("MOTION_BLOCKING".to_owned(), [0u8; DIGEST_LEN])],
        }
    }

    fn set(data_version: u32, seed: i64, chunks: Vec<ChunkDigest>) -> DigestSet {
        DigestSet {
            data_version,
            seed,
            chunks,
        }
    }

    #[test]
    fn identical_sets_diff_to_nothing() {
        let world = set(3953, 0, vec![chunk(0, 0, 1, 2), chunk(1, 0, 3, 4)]);
        let report = diff(&world, &world);
        assert!(report.is_identical());
        assert_eq!(summary_line(&report), "identical");
    }

    #[test]
    fn missing_extra_and_divergent_are_each_named_for_what_they_are() {
        let a = set(
            3953,
            5,
            vec![chunk(0, 0, 1, 1), chunk(1, 0, 1, 1), chunk(3, 0, 1, 1)],
        );
        let b = set(
            3953,
            5,
            vec![
                chunk(0, 0, 9, 9), // divergent
                chunk(2, 0, 1, 1), // extra
                                   // (1,0) and (3,0) missing
            ],
        );
        let report = diff(&a, &b);
        assert_eq!(report.rows.len(), 4, "one row per non-identical coordinate");
        assert_eq!(report.rows[0].kind(), "divergent");
        assert_eq!((report.rows[0].x, report.rows[0].z), (0, 0));
        assert_eq!(report.rows[0].differing_parts(), vec!["blocks", "biomes"]);
        assert_eq!(report.rows[1].kind(), "missing", "(1,0) is in A alone");
        assert_eq!(report.rows[2].kind(), "extra", "(2,0) is in B alone");
        assert_eq!(report.rows[3].kind(), "missing", "(3,0) is in A alone too");
        assert_eq!(
            summary_line(&report),
            "1 divergent, 2 missing, 1 extra of the compared chunks"
        );
    }

    #[test]
    fn a_heightmap_change_is_reported_as_a_heightmap_part_only() {
        let mut a = chunk(0, 0, 1, 1);
        let b = chunk(0, 0, 1, 1);
        a.heightmaps[0].1 = [1; DIGEST_LEN];
        // b keeps the zeroed map, so only the heightmap moved.
        assert_eq!(
            Divergence {
                x: 0,
                z: 0,
                a: Some(a),
                b: Some(b)
            }
            .differing_parts(),
            vec!["heightmap"]
        );

        // And when every part agrees there are no parts to list.
        assert_eq!(
            Divergence {
                x: 0,
                z: 0,
                a: Some(chunk(0, 0, 1, 1)),
                b: Some(chunk(0, 0, 1, 1))
            }
            .differing_parts(),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn a_heightmap_present_on_one_side_only_is_a_difference() {
        let mut a = chunk(0, 0, 1, 1);
        a.heightmaps
            .push(("WORLD_SURFACE".to_owned(), [2; DIGEST_LEN]));
        let b = chunk(0, 0, 1, 1);
        assert_eq!(
            Divergence {
                x: 0,
                z: 0,
                a: Some(a),
                b: Some(b)
            }
            .differing_parts(),
            vec!["heightmap"]
        );
    }

    #[test]
    fn rows_come_out_in_coordinate_order_even_from_unsorted_sides() {
        // Inputs are guaranteed sorted by the reader; the merge must keep its
        // end of that bargain regardless of how the sides interleave.
        let a = set(3953, 0, vec![chunk(-5, 0, 1, 1), chunk(0, 0, 1, 1)]);
        let b = set(3953, 0, vec![chunk(-6, 0, 1, 1)]);
        let coords: Vec<(i32, i32)> = diff(&a, &b).rows.into_iter().map(|r| (r.x, r.z)).collect();
        assert_eq!(coords, vec![(-6, 0), (-5, 0), (0, 0)]);
    }

    #[test]
    fn different_seeds_are_refused_rather_than_diffed() {
        let err = refuse_mismatched_context(
            &set(3953, 0, vec![]),
            Path::new("/cache/a"),
            &set(3953, 1, vec![]),
            Path::new("/cache/b"),
        )
        .expect_err("refused");
        assert!(err.contains("seed"), "{err}");
    }

    #[test]
    fn different_data_versions_are_refused_rather_than_diffed() {
        let err = refuse_mismatched_context(
            &set(3953, 0, vec![]),
            Path::new("/cache/a"),
            &set(3700, 0, vec![]),
            Path::new("/cache/b"),
        )
        .expect_err("refused");
        assert!(err.contains("data version"), "{err}");
    }

    #[test]
    fn parse_takes_two_sets_and_an_optional_tsv_target() {
        let parsed = parse(&["/cache/one".to_owned(), "/cache/two".to_owned()]).expect("two paths");
        assert_eq!(parsed.a, PathBuf::from("/cache/one"));
        assert_eq!(parsed.tsv, None);

        let parsed = parse(&[
            "--tsv".to_owned(),
            "/tmp/diff.tsv".to_owned(),
            "/one".to_owned(),
            "/two".to_owned(),
        ])
        .expect("with tsv");
        assert_eq!(parsed.tsv, Some(PathBuf::from("/tmp/diff.tsv")));

        assert!(parse(&["/only-one".to_owned()]).is_err());
        assert!(parse(&[]).is_err());
        assert!(parse(&["/a".to_owned(), "/b".to_owned(), "/c".to_owned()]).is_err());
    }

    #[test]
    fn the_tsv_names_every_row_with_both_block_digests() {
        let report = DiffReport {
            rows: vec![
                Divergence {
                    x: -1,
                    z: 4,
                    a: Some(chunk(-1, 4, 0xaa, 1)),
                    b: None,
                },
                Divergence {
                    x: 0,
                    z: 0,
                    a: Some(chunk(0, 0, 0x01, 1)),
                    b: Some(chunk(0, 0, 0x02, 1)),
                },
            ],
        };
        let tsv = render_tsv(&report);
        let lines: Vec<&str> = tsv.lines().collect();
        assert_eq!(lines.len(), 3, "{tsv}");
        assert!(lines[0].starts_with("# kind"), "{tsv}");
        assert_eq!(
            lines[1],
            format!("missing\t-1\t4\t{}\t{}\t", "", hex(&[0xaa; DIGEST_LEN]))
        );
        // Only the block digest moved; the biomes agree on both sides.
        assert_eq!(
            lines[2],
            format!(
                "divergent\t0\t0\tblocks\t{}\t{}",
                hex(&[0x01; DIGEST_LEN]),
                hex(&[0x02; DIGEST_LEN])
            )
        );
    }

    #[test]
    fn the_exit_code_contract_distinguishes_finding_from_failure() {
        // Documented here as constants so the contract cannot drift silently
        // away from what CI scripts against it.
        assert_ne!(EXIT_DIFFERENT, EXIT_UNRUNNABLE);
        assert_eq!(EXIT_DIFFERENT, 1);
        assert_eq!(EXIT_UNRUNNABLE, 2);
    }
}

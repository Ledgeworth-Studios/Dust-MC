//! How much of Minecraft's placement Dust reproduces, as a number.
//!
//! # What this is for
//!
//! Decision record 0011 chose rules in Dust over a table on the operator's
//! disk, and said the rules are worth exactly what their check says they are
//! worth. This is the check. It reads the answers `tools/bot/placement.js`
//! asked of a real server, asks Dust the same questions, and counts.
//!
//! It is a **measurement and not a gate**, exactly as `harness light` is: a
//! verb that failed for a known gap would be red on every run and read by
//! nobody. What it produces is a number that goes down as rules are written,
//! and a list of what is still wrong.
//!
//! # Where the answers come from
//!
//! Not from here. They are Minecraft's, so they live in the harness cache on
//! the operator's own disk beside everything else the extractor produces —
//! `tools/bot/README.md` is how to make them. Without a file this verb says so
//! and stops; it does not guess.
//!
//! # What it is comparing against
//!
//! `dust_sim::placement`, which is the same function the server calls. This
//! file deliberately does not reimplement it: a checker with its own copy of
//! the rule agrees with itself under any rule, including a wrong one. A rule
//! written there shows up here without this being edited, which is the whole
//! arrangement.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dust_registry::{Block, BlockConstants, BlockState, Item, ItemBlocks};
use dust_sim::placement::{Around, Click, Face, Solid};

/// What `harness placement` was asked to do.
#[derive(Debug)]
pub struct Options {
    /// The answers, as `tools/bot/placement.js` wrote them.
    pub answers: PathBuf,
    /// The item-to-block table, which says which block an item puts down.
    pub items: Option<PathBuf>,
    /// The constants table, which says which of a block's faces are full —
    /// the one thing a connection rule reads and the block table cannot say.
    pub constants: Option<PathBuf>,
    /// Print every disagreement rather than a sample of each kind.
    pub verbose: bool,
}

/// One row of the answers file.
#[derive(Debug)]
struct Answer {
    item: String,
    face: u8,
    yaw: f32,
    pitch: f32,
    cursor_y: String,
    /// The state Minecraft put down, `REFUSED`, or a row the tool could not
    /// take.
    result: Outcome,
    /// What the six cells around the target held when the placement went out,
    /// as `north=minecraft:stone;up=…`. Empty for a row from the grid survey,
    /// which varied the click and never the surroundings — see
    /// [`neighbourhood`] for what that is read as.
    before: String,
    /// Which of those six the placement *changed*, in the same spelling. This
    /// is the second half of a neighbour rule and the half a survey of placed
    /// states alone cannot see: a fence has to connect when the block beside it
    /// arrives later, not only when it was there first.
    after: String,
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// A state, in `minecraft:name[a=b,c=d]` spelling.
    Placed(String),
    /// Minecraft placed nothing.
    Refused,
    /// The tool's own arena did not settle. Not a finding about either server,
    /// and counted apart so that it cannot quietly become one.
    Unmeasured,
}

/// Read the answers, ask Dust the same questions, and print the score.
pub fn run(options: &Options) -> ExitCode {
    match measure(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("error: {why}");
            ExitCode::from(2)
        }
    }
}

fn measure(options: &Options) -> Result<(), String> {
    let text = std::fs::read_to_string(&options.answers)
        .map_err(|e| format!("could not read {}: {e}", options.answers.display()))?;
    let answers = parse_answers(&text)?;
    if answers.is_empty() {
        return Err(format!(
            "{} holds no rows; see tools/bot/README.md for how to make one",
            options.answers.display()
        ));
    }

    let items = load_items(options.items.as_deref())?;
    let constants = load_constants(options.constants.as_deref())?;
    let solid = constants.as_ref().and_then(Solid::from_constants);
    if solid.is_none() {
        println!(
            "note: no table saying which of a block's faces are full, so no connection rule \
             runs — a fence, a wall and a pane come out with no arms at all. \
             `cargo xtask extract --only constants` writes one."
        );
    }

    let mut score = Score::default();
    for answer in &answers {
        score.add(answer, &items, solid);
    }
    // A file where nothing resolved is a file in the wrong shape, and printing
    // `0 of 0 agree` for it is a measurement of nothing dressed as a clean run.
    // The commonest cause is the one that produced this check: an answers file
    // whose items are spelled without their namespace.
    if score.agreed + score.disagreed == 0 && score.unresolved > 0 {
        return Err(format!(
            "none of the {} placements name an item this build has — the first is {:?}. \
             An item is `minecraft:stone` here and not `stone`; a file written by an older \
             tools/bot/placement.js spells them the short way and has to be made again.",
            score.unresolved,
            score.first_unresolved.as_deref().unwrap_or("")
        ));
    }
    score.report(options.verbose);
    Ok(())
}

/// Everything counted, and enough of what disagreed to act on.
/// One disagreement, kept to be printed.
#[derive(Debug)]
struct Example {
    /// What Dust would have.
    dust: String,
    /// What Minecraft had.
    vanilla: String,
    /// What was around the cell when it happened. Empty for a grid row, where
    /// the answer is always "the support, opposite the clicked face".
    around: String,
    /// How many rows of this item disagreed.
    count: u64,
}

#[derive(Debug, Default)]
struct Score {
    /// Situations where Minecraft placed a state and Dust would place the same.
    agreed: u64,
    /// Situations where it would place a different one.
    disagreed: u64,
    /// Situations Minecraft refused. Dust's own refusals are a different
    /// question — placement *rules*, not placement *state* — and are not what
    /// this verb measures.
    refused: u64,
    /// Rows the measuring tool could not take.
    unmeasured: u64,
    /// Items whose block this build cannot resolve at all.
    unresolved: u64,
    /// The first of them, for the message that names what is wrong.
    first_unresolved: Option<String>,
    /// One example per item — what Dust would place, what Minecraft placed, the
    /// neighbourhood it happened in, and how many rows share the item — so a
    /// run prints a worklist rather than ten thousand rows.
    disagreements: BTreeMap<String, Example>,
    /// Which items ever placed anything, and which of those ever disagreed.
    placing: BTreeMap<String, bool>,
    /// Neighbours the placement changed, and whether Dust would change them
    /// the same way. Counted apart from the placed cell because they are a
    /// different rule reaching the world down a different path — a fence that
    /// connects when it is put down beside another and not when another is put
    /// down beside it is connected in one direction, which looks worse than
    /// not connecting at all.
    neighbours_agreed: u64,
    neighbours_disagreed: u64,
    /// The same, for what the placement did to the cells around it.
    neighbour_disagreements: BTreeMap<String, Example>,
    /// Sides that were in the scene a tick before the click and gone a tick
    /// after it, so the click read air where the survey wrote a block. See
    /// [`emptied`].
    emptied: u64,
}

impl Score {
    fn add(&mut self, answer: &Answer, items: &Option<ItemBlocks>, solid: Option<Solid>) {
        let expected = match &answer.result {
            Outcome::Unmeasured => {
                self.unmeasured += 1;
                return;
            }
            Outcome::Refused => {
                self.refused += 1;
                return;
            }
            Outcome::Placed(state) => state,
        };

        self.emptied += emptied(answer);
        self.add_neighbours(answer, items, solid);
        let Some(theirs) = dust_state(answer, items, solid) else {
            self.unresolved += 1;
            self.first_unresolved
                .get_or_insert_with(|| answer.item.clone());
            return;
        };
        let seen = self.placing.entry(answer.item.clone()).or_insert(false);
        if theirs == *expected {
            self.agreed += 1;
            return;
        }
        *seen = true;
        self.disagreed += 1;
        let entry = self
            .disagreements
            .entry(answer.item.clone())
            .or_insert_with(|| Example {
                dust: theirs.clone(),
                vanilla: expected.clone(),
                around: answer.before.clone(),
                count: 0,
            });
        entry.count += 1;
        // The example kept is the first one. Which situation it was is in the
        // answers file; what is wanted here is the *shape* of the difference,
        // and one row of each shape is a worklist.
        let _ = (&answer.face, &answer.yaw, &answer.pitch, &answer.cursor_y);
    }

    /// Score the other half: what the placement did to the cells around it.
    ///
    /// Dust's answer for one of them is `shaped` run on the neighbour with the
    /// placed block on the side it arrived from. **That is the whole
    /// neighbourhood and not a simplification of it**: the neighbour survey
    /// stands each neighbour alone in a cleared volume with a stone floor, so
    /// its other three sides and the cell above it really are air, and a rule
    /// that read them would be reading air.
    fn add_neighbours(
        &mut self,
        answer: &Answer,
        items: &Option<ItemBlocks>,
        solid: Option<Solid>,
    ) {
        if answer.after.is_empty() || answer.after == "-" {
            return;
        }
        let Some(placed) = dust_state_id(answer, items, solid) else {
            return;
        };
        let Some(solid) = solid else { return };
        for (side, expected) in neighbours(&answer.after) {
            // The cell the placement went into is not a neighbour of itself,
            // and a door's upper half is a *second placement* rather than a
            // shape rule — nothing here can put a block somewhere the click did
            // not name, and counting it as a wrong shape would say this rule
            // was wrong about something that is not its question.
            let was = neighbourhood(answer).at(side);
            if was == air() {
                // A cell that held nothing and holds something now was not
                // *shaped* by the placement, it was written by it — a door's
                // upper half, and nothing else in this survey. Dust places one
                // block per click and says so; that is a different gap with a
                // different fix, and scoring it here would report this rule as
                // wrong about a question it was never asked.
                continue;
            }
            let ours = dust_sim::placement::shaped(
                was,
                Around::empty().with(side.opposite(), placed),
                solid,
            );
            let rendered = render(ours);
            if rendered == expected {
                self.neighbours_agreed += 1;
                continue;
            }
            self.neighbours_disagreed += 1;
            let entry = self
                .neighbour_disagreements
                .entry(answer.item.clone())
                .or_insert_with(|| Example {
                    dust: rendered,
                    vanilla: expected,
                    around: answer.before.clone(),
                    count: 0,
                });
            entry.count += 1;
        }
    }

    fn report(&self, verbose: bool) {
        let measured = self.agreed + self.disagreed;
        println!("placement, against Minecraft's own answers");
        println!();
        println!(
            "  {:>7}  situations where Minecraft placed a block",
            measured
        );
        println!(
            "  {:>7}  of them Dust would place the same state ({})",
            self.agreed,
            percent(self.agreed, measured)
        );
        println!(
            "  {:>7}  of them it would not ({})",
            self.disagreed,
            percent(self.disagreed, measured)
        );
        println!();
        println!(
            "  {:>7}  Minecraft refused, so there is nothing to compare",
            self.refused
        );
        if self.unmeasured > 0 {
            println!(
                "  {:>7}  the measuring tool could not take, and they are not a finding",
                self.unmeasured
            );
        }
        if self.unresolved > 0 {
            println!(
                "  {:>7}  name an item this build has no block for",
                self.unresolved
            );
        }

        let items = self.placing.len();
        let wrong = self.placing.values().filter(|w| **w).count();
        println!();
        println!(
            "  {wrong} of the {items} items that placed anything come out wrong in at least \
             one situation"
        );

        // The other half, and printed as its own block because it is measured
        // against its own rows. A survey that never varied the surroundings
        // carries none of these and this says nothing.
        let touched = self.neighbours_agreed + self.neighbours_disagreed;
        if touched > 0 {
            println!();
            println!("  {:>7}  neighbours the placement changed", touched);
            println!(
                "  {:>7}  of them Dust would change the same way ({})",
                self.neighbours_agreed,
                percent(self.neighbours_agreed, touched)
            );
            println!(
                "  {:>7}  of them it would not ({})",
                self.neighbours_disagreed,
                percent(self.neighbours_disagreed, touched)
            );
        }
        if self.emptied > 0 {
            println!(
                "  {:>7}  neighbour(s) fell over as the click landed, and are read as the \
                 air they left",
                self.emptied
            );
        }

        // Each list is guarded on its own. An early return on the first one
        // hid the second entirely the day the first went empty, which is the
        // day the second is the only thing left worth reading.
        if !self.disagreements.is_empty() {
            self.report_placed(verbose);
        }
        if !self.neighbour_disagreements.is_empty() {
            self.report_neighbours(verbose);
        }
    }

    fn report_placed(&self, verbose: bool) {
        println!();
        println!("what is still wrong, one line per item:");
        let mut rows: Vec<(&String, &Example)> = self.disagreements.iter().collect();
        rows.sort_by_key(|(item, example)| (std::cmp::Reverse(example.count), (*item).clone()));
        let shown = if verbose {
            rows.len()
        } else {
            rows.len().min(25)
        };
        for (item, example) in rows.iter().take(shown) {
            print_example(item, example);
        }
        if shown < rows.len() {
            println!(
                "  … and {} more items; pass --verbose for all of them",
                rows.len() - shown
            );
        }
    }

    fn report_neighbours(&self, verbose: bool) {
        println!();
        println!("what a placement still does not do to what is beside it:");
        let mut rows: Vec<(&String, &Example)> = self.neighbour_disagreements.iter().collect();
        rows.sort_by_key(|(item, example)| (std::cmp::Reverse(example.count), (*item).clone()));
        let shown = if verbose {
            rows.len()
        } else {
            rows.len().min(10)
        };
        for (item, example) in rows.iter().take(shown) {
            print_example(item, example);
        }
        if shown < rows.len() {
            println!(
                "  … and {} more items; pass --verbose for all of them",
                rows.len() - shown
            );
        }
    }
}

/// One disagreement, with the neighbourhood it happened in.
///
/// The neighbourhood is the line that makes the difference actionable: "a fence
/// is wrong" is a sentence, and "a fence beside a bottom slab is wrong" is a
/// thing to go and fix.
fn print_example(item: &str, example: &Example) {
    println!("  {:>4}x {item}", example.count);
    println!("         dust {}", example.dust);
    println!("         them {}", example.vanilla);
    if !example.around.is_empty() && example.around != "-" {
        println!("         with {}", example.around);
    }
}

fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "—".to_owned();
    }
    format!("{:.1}%", (part as f64 / whole as f64) * 100.0)
}

/// The state Dust would put down for an item, in the answers file's spelling.
///
/// `None` when the item is one this build has no block for, which is a version
/// skew between the answers and the build rather than a disagreement.
fn dust_state(answer: &Answer, items: &Option<ItemBlocks>, solid: Option<Solid>) -> Option<String> {
    dust_state_id(answer, items, solid).map(render)
}

/// The same, as a state.
///
/// Two steps and they are the two rules: [`state_for`] reads the click, and
/// [`shaped`] reads what is beside the cell. They are separate here for the
/// same reason they are separate in `dust_sim` — the second one also runs when
/// there was no click at all.
///
/// [`state_for`]: dust_sim::placement::state_for
/// [`shaped`]: dust_sim::placement::shaped
fn dust_state_id(
    answer: &Answer,
    items: &Option<ItemBlocks>,
    solid: Option<Solid>,
) -> Option<BlockState> {
    let item = Item::from_name(&answer.item)?;
    let block = match items {
        // With the item table, the block is Minecraft's own answer to "what
        // does this item place".
        Some(table) => table.places(item)?,
        // Without it, the name — which is right about 909 of the 925 placing
        // items and wrong about sixteen, exactly as decision record 0008 says.
        // Reported rather than silently assumed: `measure` says which it used.
        None => Block::from_name(item.name())?,
    };
    let click = Click {
        face: Face::from_protocol(answer.face)?,
        cursor_y: answer.cursor_y.parse().ok()?,
        yaw: answer.yaw,
        pitch: answer.pitch,
    };
    let placed = dust_sim::placement::state_for(block, click);
    let Some(solid) = solid else {
        return Some(placed);
    };
    Some(dust_sim::placement::shaped(
        placed,
        neighbourhood(answer),
        solid,
    ))
}

/// What was around the cell the placement landed in.
///
/// **A row with no `before` column is not a row with no neighbours.** The grid
/// survey clears a volume and puts one stone block back — the support the click
/// was aimed at — so the placement lands beside exactly one block, on the side
/// opposite the face that was clicked, with air on the other five. That is
/// where the fifty-five fences, walls and panes in the grid's own wrong list
/// come from: they are wrong about the *support*, and a scorer that read those
/// rows as "nothing was beside it" would call them all correct.
fn neighbourhood(answer: &Answer) -> Around {
    let mut around = Around::empty();
    if answer.before.is_empty() || answer.before == "-" {
        let Some(face) = Face::from_protocol(answer.face) else {
            return around;
        };
        let stone = Block::from_name("minecraft:stone").expect("this build has stone");
        return around.with(face.opposite(), stone.default_state());
    }
    for (side, state) in neighbours(&answer.before) {
        if let Some(state) = parse_state(&state) {
            around = around.with(side, state);
        }
    }
    // **A cell the placement emptied was not there for the click to read.**
    // `before` is measured a tick ahead of the click and `after` a tick behind
    // it, so a block that falls in between is in the first column and not in
    // the second: a ladder in the arena hangs on the cell the block is about to
    // go into, and drops the moment it is asked to hold itself up. Minecraft's
    // own answer for that row is a fence with no arm, and a scorer that read
    // the ladder out of `before` would call that a wrong connection rule — one
    // row in 799 of a fence-against-every-block run, and it looked exactly like
    // one.
    for (side, state) in neighbours(&answer.after) {
        if fell(&state) {
            around = around.with(side, air());
        }
    }
    around
}

/// How many sides of this row the placement emptied.
///
/// Counted and printed rather than quietly folded into the agreement, because
/// a row whose neighbourhood fell over during the click is a row measured
/// against a scene that was not fully there — and a survey where that number
/// grew would be one to go and look at rather than to read the percentage of.
fn emptied(answer: &Answer) -> u64 {
    let before: Vec<Face> = neighbours(&answer.before)
        .into_iter()
        .filter(|(_, state)| !fell(state))
        .map(|(side, _)| side)
        .collect();
    neighbours(&answer.after)
        .into_iter()
        .filter(|(side, state)| fell(state) && before.contains(side))
        .count() as u64
}

/// The `north=minecraft:stone;up=…` spelling, read back.
///
/// A pair the reader does not understand is dropped rather than refused, which
/// is the one place this file is lenient and is worth saying why: the field
/// carries whatever the *game* had in those cells, which on a version this
/// build does not know includes blocks it has no name for. A row naming one is
/// a version skew, and the same skew already has a count of its own.
fn neighbours(field: &str) -> Vec<(Face, String)> {
    field
        .split(';')
        .filter_map(|pair| pair.split_once('='))
        .filter_map(|(side, state)| Face::from_direction(side).map(|side| (side, state.to_owned())))
        .collect()
}

/// A state from `minecraft:name[a=b,c=d]`.
///
/// Built by setting one property at a time on the block's default, which is the
/// same door `dust_sim` puts its own answers through. A property this build
/// does not have makes the whole state `None` rather than a state missing it:
/// a neighbourhood read as a *different block state* than the one that was
/// there is worse than a row that does not count.
fn parse_state(text: &str) -> Option<BlockState> {
    let (name, rest) = match text.split_once('[') {
        Some((name, rest)) => (name, rest.strip_suffix(']')?),
        None => (text, ""),
    };
    let mut state = Block::from_name(name)?.default_state();
    for property in rest.split(',').filter(|p| !p.is_empty()) {
        let (name, value) = property.split_once('=')?;
        state = state.with(name, value)?;
    }
    Some(state)
}

/// Whether a cell the placement changed was **emptied** rather than reshaped.
///
/// Air is the obvious spelling and it is not the only one. A block that was
/// waterlogged leaves its water behind when it falls, so a coral fan beside a
/// fence reads back as `minecraft:water[level=0]`; fourteen of the twenty rows
/// a fence-against-every-block run first reported as wrong shapes were that,
/// and every one of them was the support rule rather than this one.
fn fell(state: &str) -> bool {
    matches!(
        state.split('[').next().unwrap_or(state),
        "minecraft:air"
            | "minecraft:cave_air"
            | "minecraft:void_air"
            | "minecraft:water"
            | "minecraft:lava"
    )
}

/// Air, for telling "this cell was shaped" from "this cell was written into".
fn air() -> BlockState {
    Block::from_name("minecraft:air")
        .expect("every version of the game has air")
        .default_state()
}

/// A state, in the answers file's spelling.
fn render(state: dust_registry::BlockState) -> String {
    let mut properties: Vec<String> = state
        .properties()
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    // Sorted by name, which is what the measuring tool does. The state's own
    // order is the property order of the block table and means nothing to a
    // reader; agreeing on *an* order is what lets two strings be compared.
    properties.sort();
    let name = state.block().name();
    if properties.is_empty() {
        name.to_owned()
    } else {
        format!("{name}[{}]", properties.join(","))
    }
}

/// Read the item-to-block table, from the given path or the extract cache.
fn load_items(given: Option<&Path>) -> Result<Option<ItemBlocks>, String> {
    let path = match given {
        Some(path) => path.to_path_buf(),
        None => {
            let Some(found) = default_table("items.tsv") else {
                println!(
                    "note: no item-to-block table found, so an item's block is taken from its \
                     name — right about 909 of the 925 placing items and wrong about sixteen. \
                     Pass --items to use the real one."
                );
                return Ok(None);
            };
            found
        }
    };
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let table = ItemBlocks::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    println!(
        "items: {} placements from {}",
        table.placing(),
        path.display()
    );
    Ok(Some(table))
}

/// Read the constants table, from the given path or the extract cache.
///
/// It carries the one thing a connection rule needs that the block table cannot
/// say: which of a block state's six faces are full squares. Without it the
/// rules do not run, which is stated rather than approximated — see [`Solid`].
fn load_constants(given: Option<&Path>) -> Result<Option<BlockConstants>, String> {
    let path = match given {
        Some(path) => path.to_path_buf(),
        None => match default_table("constants.tsv") {
            Some(found) => found,
            None => return Ok(None),
        },
    };
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let table = BlockConstants::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    println!(
        "constants: {} block states from {}",
        table.len(),
        path.display()
    );
    Ok(Some(table))
}

/// The newest table of a given name the extractor has written, if there is one.
fn default_table(name: &str) -> Option<PathBuf> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .join(".dust-extract");
    let mut found: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(name))
        .filter(|path| path.is_file())
        .collect();
    found.sort();
    found.pop()
}

/// Read the answers file.
///
/// Rows the reader does not understand are an error rather than a skip: a file
/// half of which was silently dropped would report a score for the other half
/// and call it the score.
fn parse_answers(text: &str) -> Result<Vec<Answer>, String> {
    let mut out = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = index + 1;
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 6 {
            return Err(format!(
                "line {at}: {} field(s) where an answer has at least six",
                fields.len()
            ));
        }
        let number = |what: &str, text: &str| -> Result<f32, String> {
            text.parse()
                .map_err(|_| format!("line {at}: {what} is {text:?}, which is not a number"))
        };
        let result = match fields[5] {
            "REFUSED" => Outcome::Refused,
            // Air is the other spelling of a refusal: the client predicted a
            // block and the server answered with what is really in the cell,
            // which for one the arena just cleared is air. The measuring tool
            // makes the same substitution — this is the same rule applied on
            // the reading side rather than a workaround for the writing one,
            // and it is what keeps a file written by a tool that forgot from
            // scoring a thousand refusals as a thousand wrong answers.
            "minecraft:air" | "air" => Outcome::Refused,
            other if other.starts_with("ARENA") => Outcome::Unmeasured,
            state => Outcome::Placed(state.to_owned()),
        };
        out.push(Answer {
            before: fields.get(7).copied().unwrap_or_default().to_owned(),
            after: fields.get(8).copied().unwrap_or_default().to_owned(),
            item: fields[0].to_owned(),
            face: fields[1]
                .parse::<u8>()
                .ok()
                .filter(|face| *face < 6)
                .ok_or_else(|| format!("line {at}: face {} is not one of the six", fields[1]))?,
            yaw: number("yaw", fields[2])?,
            pitch: number("pitch", fields[3])?,
            cursor_y: fields[4].to_owned(),
            result,
        });
    }
    Ok(out)
}

/// Parse this verb's arguments.
pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut answers = None;
    let mut items = None;
    let mut constants = None;
    let mut verbose = false;
    let mut seen: Vec<(&'static str, String)> = Vec::new();
    let mut at = 0;
    while at < args.len() {
        match args[at].as_str() {
            "--answers" => {
                at = super::take_value(&mut seen, "--answers", args, at + 1)?;
                answers = Some(PathBuf::from(&seen.last().expect("just stored").1));
            }
            "--items" => {
                at = super::take_value(&mut seen, "--items", args, at + 1)?;
                items = Some(PathBuf::from(&seen.last().expect("just stored").1));
            }
            "--constants" => {
                at = super::take_value(&mut seen, "--constants", args, at + 1)?;
                constants = Some(PathBuf::from(&seen.last().expect("just stored").1));
            }
            "--verbose" => {
                verbose = true;
                at += 1;
            }
            other => return Err(format!("placement does not take {other:?}")),
        }
    }
    Ok(Options {
        answers: answers.ok_or(
            "placement needs --answers <file>, written by tools/bot/placement.js; \
             see tools/bot/README.md",
        )?,
        items,
        constants,
        verbose,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_renders_the_way_the_measuring_tool_writes_one() {
        // The two sides meet on this string and nothing checks it but this
        // test. Namespaced, properties in name order, comma separated, square
        // brackets — and no brackets at all for a block that has no
        // properties, because `stone[]` is a different string from `stone`.
        let stone = Block::from_name("minecraft:stone").expect("this build has stone");
        assert_eq!(render(stone.default_state()), "minecraft:stone");

        let stairs = Block::from_name("minecraft:oak_stairs").expect("this build has stairs");
        let rendered = render(stairs.default_state());
        assert!(rendered.starts_with("minecraft:oak_stairs["), "{rendered}");
        assert!(rendered.ends_with(']'), "{rendered}");
        let inside = rendered
            .trim_start_matches("minecraft:oak_stairs[")
            .trim_end_matches(']');
        let names: Vec<&str> = inside
            .split(',')
            .map(|kv| kv.split('=').next().unwrap())
            .collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "properties are in name order: {rendered}");
    }

    #[test]
    fn the_three_outcomes_are_told_apart() {
        let text = "\
# item\tface\tyaw\tpitch\tcursor_y\tresult\tsurvived
minecraft:stone\t1\t0\t0\t0.25\tminecraft:stone\tstood
minecraft:torch\t0\t0\t0\t0.25\tREFUSED\t-
minecraft:torch\t4\t0\t90\t0.25\tARENA DID NOT SETTLE\t-
";
        let answers = parse_answers(text).expect("three well-formed rows");
        assert_eq!(answers.len(), 3);
        assert_eq!(
            answers[0].result,
            Outcome::Placed("minecraft:stone".to_owned())
        );
        assert_eq!(answers[1].result, Outcome::Refused);
        // Not a finding about either server, and told apart from a refusal so
        // that it cannot quietly become one.
        assert_eq!(answers[2].result, Outcome::Unmeasured);
    }

    #[test]
    fn air_is_the_other_spelling_of_a_refusal() {
        // No item places air, so a row saying air is a row where nothing was
        // placed. Scored as a wrong answer instead, one measuring tool's
        // oversight becomes a thousand findings about a server that did
        // nothing wrong.
        let text = "\
# item\tface\tyaw\tpitch\tcursor_y\tresult\tsurvived
minecraft:allium\t1\t0\t0\t0.25\tminecraft:air\tstood
";
        let answers = parse_answers(text).expect("one well-formed row");
        assert_eq!(answers[0].result, Outcome::Refused);
    }

    #[test]
    fn a_row_the_reader_does_not_understand_stops_the_run() {
        // Skipping it would report a score for whatever was left and call it
        // the score.
        let text = "# item\tface\nminecraft:stone\t1\n";
        assert!(parse_answers(text).is_err());
    }

    #[test]
    fn a_run_with_no_item_table_falls_back_to_the_name() {
        // And is right about stone, which is one of the 909 the fallback gets
        // right. The sixteen it does not are why `--items` exists.
        let placed = |item: &str| {
            dust_state(
                &Answer {
                    item: item.to_owned(),
                    face: 1,
                    yaw: 0.0,
                    pitch: 0.0,
                    cursor_y: "0.25".to_owned(),
                    result: Outcome::Refused,
                    before: String::new(),
                    after: String::new(),
                },
                &None,
                None,
            )
        };
        assert_eq!(
            placed("minecraft:stone"),
            Some("minecraft:stone".to_owned())
        );
        assert_eq!(placed("minecraft:diamond_sword"), None);
    }
}

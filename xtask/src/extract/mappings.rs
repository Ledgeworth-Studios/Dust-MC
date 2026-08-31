//! Mojang's published mappings, read well enough to call into an obfuscated jar.
//!
//! # What this is for
//!
//! Some of what Dust needs from Minecraft is in no `--reports` output and no
//! data pack, because it is a constant in Java code: a block's opacity, its
//! light emission, the sound it makes. Decision record 0008 is the standing
//! account of that. The only source for those numbers that is not an invention
//! is the jar itself, and the jar is obfuscated.
//!
//! Mojang publish a mappings file beside every server jar that says which
//! obfuscated name each class, field and method was given. This reads it, so an
//! oracle can be written against *semantic* names and resolved to obfuscated
//! ones at run time — see `xtask/oracle/dustoracle/Names.java`, which loads the
//! table this produces and therefore contains no Minecraft identifier at all.
//! A version that renames every class changes the table and nothing else.
//!
//! # The format, and the one thing about it that bites
//!
//! ```text
//! net.minecraft.world.level.block.Block -> dfy:
//! # {"fileName":"Block.java","id":"sourceFile"}
//!     net.minecraft.core.IdMapper BLOCK_STATE_REGISTRY -> q
//!     875:875:net.minecraft.world.level.block.Block getBlock() -> b
//!     929:932:int getLightBlock(net.minecraft.world.level.BlockGetter,net.minecraft.core.BlockPos) -> b
//! ```
//!
//! An unindented line opens a class; indented lines are its members until the
//! next one. A member line optionally carries `first:last:` source line numbers,
//! then a return type, then the name, then a parameter list for a method.
//!
//! **An obfuscated member name is not unique within its class.** The two
//! methods above are both `b`: obfuscation reuses a name freely as long as the
//! signatures differ, exactly as Java itself allows. So a lookup keyed by name
//! alone is not a lookup — it is a coincidence that works on the members where
//! there happens to be one candidate. Methods are keyed here by **name and
//! parameter types together**, which is what the language uses to tell them
//! apart, and what `getDeclaredMethod` on the other side will be given.
//!
//! # What is deliberately not here
//!
//! No remapping, and no reading of the jar. This turns names into other names;
//! everything that runs Java is in the oracle. And **nothing from the mappings
//! file is ever committed** — it is Mojang's, it arrives beside the jar at the
//! operator's machine, and what this module produces is a table of the handful
//! of names one oracle asks for, written to the extract cache. Same rule as
//! D6 and D7.

use std::collections::BTreeMap;

/// One class's members, under the obfuscated name of the class itself.
#[derive(Debug, Default)]
struct Class {
    obfuscated: String,
    fields: BTreeMap<String, String>,
    /// Keyed by name *and* parameter types. See the module note: the value
    /// alone does not identify a member.
    methods: BTreeMap<(String, Vec<String>), String>,
}

/// A parsed mappings file.
#[derive(Debug, Default)]
pub struct Mappings {
    classes: BTreeMap<String, Class>,
}

impl Mappings {
    /// Parse a ProGuard-format mappings file.
    ///
    /// # Errors
    ///
    /// A member line before any class line, or a line that is neither a
    /// comment, a class, nor a member. Both mean the file is not what this
    /// thinks it is, and guessing past them would produce a table that is
    /// wrong in a way nothing downstream could notice.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut classes: BTreeMap<String, Class> = BTreeMap::new();
        let mut current: Option<String> = None;
        for (number, line) in text.lines().enumerate() {
            let line_number = number + 1;
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with(char::is_whitespace) {
                let source = current.as_ref().ok_or_else(|| {
                    format!("line {line_number}: a member before any class opened one")
                })?;
                let class = classes.get_mut(source).expect("just inserted");
                parse_member(line.trim(), class)
                    .map_err(|why| format!("line {line_number}: {why}"))?;
            } else {
                let (source, obfuscated) = line
                    .trim_end_matches(':')
                    .split_once(" -> ")
                    .ok_or_else(|| format!("line {line_number}: no `->` in a class line"))?;
                classes.insert(
                    source.to_owned(),
                    Class {
                        obfuscated: obfuscated.to_owned(),
                        ..Class::default()
                    },
                );
                current = Some(source.to_owned());
            }
        }
        Ok(Self { classes })
    }

    /// The obfuscated name of a class, by its source name.
    ///
    /// Inner classes are written with a `$`, as the file writes them and as
    /// `Class.forName` wants them: `BlockBehaviour$BlockStateBase`.
    #[must_use]
    pub fn class(&self, source: &str) -> Option<&str> {
        self.classes.get(source).map(|c| c.obfuscated.as_str())
    }

    /// The obfuscated name of a field.
    #[must_use]
    pub fn field(&self, class: &str, field: &str) -> Option<&str> {
        self.classes
            .get(class)?
            .fields
            .get(field)
            .map(String::as_str)
    }

    /// The obfuscated name of a method, identified by name *and* parameters.
    ///
    /// The parameters are source type names, exactly as the file spells them —
    /// fully qualified, no spaces. Passing the wrong list is a `None` rather
    /// than a near miss, which is the point: a near miss here is a call into a
    /// different method that happens to share a letter.
    #[must_use]
    pub fn method(&self, class: &str, method: &str, parameters: &[&str]) -> Option<&str> {
        let key = (
            method.to_owned(),
            parameters.iter().map(|p| (*p).to_owned()).collect(),
        );
        self.classes
            .get(class)?
            .methods
            .get(&key)
            .map(String::as_str)
    }

    /// How many classes were read. For the extractor's own report.
    #[must_use]
    pub fn len(&self) -> usize {
        self.classes.len()
    }

    /// Whether nothing was read at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }
}

/// One thing an oracle needs to be able to name.
///
/// The key is the oracle's own vocabulary — `blockstate.light_emission` — and
/// never Minecraft's, so the Java side stays free of identifiers that a version
/// bump would invalidate.
#[derive(Debug, Clone, Copy)]
pub enum Wanted<'a> {
    /// A class, by source name.
    Class { key: &'a str, class: &'a str },
    /// A field of a class.
    Field {
        key: &'a str,
        class: &'a str,
        field: &'a str,
    },
    /// A method, with the parameter types that identify it.
    Method {
        key: &'a str,
        class: &'a str,
        method: &'a str,
        parameters: &'a [&'a str],
    },
}

impl Wanted<'_> {
    /// The key this entry is written under.
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            Self::Class { key, .. } | Self::Field { key, .. } | Self::Method { key, .. } => key,
        }
    }
}

/// Everything the light oracle has to be able to name.
///
/// Decision record 0008 is the account of why this list exists: a block's
/// opacity and its light emission are constants in Java code, in no report and
/// no data pack, and the only source for them that is not an invention is the
/// jar.
///
/// Two choices in here are worth the words.
///
/// **`BLOCK_STATE_REGISTRY` rather than the `Blocks` class.** It is an
/// `IdMapper`, so walking it yields *state id to state* — the same ids
/// `dust-registry`'s generated table already uses. Reading `Blocks` instead
/// would give names, and names would need matching up, and a matching step is a
/// place for an off-by-one nobody can see. There is no matching step.
///
/// **`lightEmission` the field, not `getLightEmission()` the method.** Both are
/// here because the oracle should prefer the field and has somewhere to fall
/// back to; the field needs no arguments at all, while every `getLightBlock`
/// overload wants a level and a position. That is why `EmptyBlockGetter` is on
/// the list: it is the level Minecraft itself passes when there is no world.
pub const LIGHT_ORACLE: &[Wanted<'static>] = &[
    Wanted::Class {
        key: "block.class",
        class: "net.minecraft.world.level.block.Block",
    },
    Wanted::Field {
        key: "block.state_registry",
        class: "net.minecraft.world.level.block.Block",
        field: "BLOCK_STATE_REGISTRY",
    },
    Wanted::Class {
        key: "blockstate.class",
        class: BLOCK_STATE_BASE,
    },
    Wanted::Field {
        key: "blockstate.light_emission",
        class: BLOCK_STATE_BASE,
        field: "lightEmission",
    },
    Wanted::Method {
        key: "blockstate.get_light_emission",
        class: BLOCK_STATE_BASE,
        method: "getLightEmission",
        parameters: &[],
    },
    Wanted::Method {
        key: "blockstate.get_light_block",
        class: BLOCK_STATE_BASE,
        method: "getLightBlock",
        parameters: &[BLOCK_GETTER, BLOCK_POS],
    },
    Wanted::Method {
        key: "blockstate.can_occlude",
        class: BLOCK_STATE_BASE,
        method: "canOcclude",
        parameters: &[],
    },
    Wanted::Method {
        key: "blockstate.propagates_skylight_down",
        class: BLOCK_STATE_BASE,
        method: "propagatesSkylightDown",
        parameters: &[BLOCK_GETTER, BLOCK_POS],
    },
    Wanted::Class {
        key: "block_getter.class",
        class: BLOCK_GETTER,
    },
    Wanted::Class {
        key: "empty_block_getter.class",
        class: EMPTY_BLOCK_GETTER,
    },
    Wanted::Field {
        key: "empty_block_getter.instance",
        class: EMPTY_BLOCK_GETTER,
        field: "INSTANCE",
    },
    Wanted::Class {
        key: "blockpos.class",
        class: BLOCK_POS,
    },
    Wanted::Field {
        key: "blockpos.zero",
        class: BLOCK_POS,
        field: "ZERO",
    },
    Wanted::Class {
        key: "idmapper.class",
        class: ID_MAPPER,
    },
    Wanted::Method {
        key: "idmapper.get_id",
        class: ID_MAPPER,
        method: "getId",
        parameters: &["java.lang.Object"],
    },
    // Before `Bootstrap` will run at all. Minecraft's own `Main` calls this
    // first, and without it static initialisation dies on "Game version not
    // set" from inside a class the stack trace names only by its obfuscated
    // letter.
    //
    // It is also the sharpest case for keying methods by their parameters:
    // `tryDetectVersion()` and `setVersion(WorldVersion)` are **both `a`** on
    // this class, and only the empty parameter list tells the oracle which of
    // them it is asking for.
    Wanted::Class {
        key: "sharedconstants.class",
        class: SHARED_CONSTANTS,
    },
    Wanted::Method {
        key: "sharedconstants.try_detect_version",
        class: SHARED_CONSTANTS,
        method: "tryDetectVersion",
        parameters: &[],
    },
    Wanted::Class {
        key: "bootstrap.class",
        class: "net.minecraft.server.Bootstrap",
    },
    Wanted::Method {
        key: "bootstrap.boot",
        class: "net.minecraft.server.Bootstrap",
        method: "bootStrap",
        parameters: &[],
    },
];

const BLOCK_STATE_BASE: &str =
    "net.minecraft.world.level.block.state.BlockBehaviour$BlockStateBase";
const BLOCK_GETTER: &str = "net.minecraft.world.level.BlockGetter";
const BLOCK_POS: &str = "net.minecraft.core.BlockPos";
const EMPTY_BLOCK_GETTER: &str = "net.minecraft.world.level.EmptyBlockGetter";
const ID_MAPPER: &str = "net.minecraft.core.IdMapper";
const SHARED_CONSTANTS: &str = "net.minecraft.SharedConstants";

/// Resolve a list of wanted names into a properties file for the Java side.
///
/// # Errors
///
/// Any entry that does not resolve, **naming every one of them rather than the
/// first**. A version bump that renames or removes a member should produce one
/// list to work through, not one failure per run — and a partial table would
/// let the oracle start and fail somewhere less obvious.
pub fn properties(mappings: &Mappings, wanted: &[Wanted<'_>]) -> Result<String, String> {
    let mut out = String::from(
        "# Generated by `cargo xtask extract`. Semantic keys to obfuscated names,\n\
         # resolved from the mappings published beside this version's server jar.\n\
         # Not committed: the names are Mojang's. See decision record 0008.\n",
    );
    let mut missing = Vec::new();
    for entry in wanted {
        let resolved = match entry {
            Wanted::Class { class, .. } => mappings.class(class).map(ToOwned::to_owned),
            Wanted::Field { class, field, .. } => {
                mappings.field(class, field).map(ToOwned::to_owned)
            }
            Wanted::Method {
                class,
                method,
                parameters,
                ..
            } => mappings
                .method(class, method, parameters)
                .map(ToOwned::to_owned),
        };
        match resolved {
            Some(name) => out.push_str(&format!("{}={name}\n", entry.key())),
            None => missing.push(entry.key().to_owned()),
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "these mappings do not name {} thing(s) the oracle needs: {}",
            missing.len(),
            missing.join(", ")
        ));
    }
    Ok(out)
}

/// Read one indented line into the class it belongs to.
fn parse_member(line: &str, class: &mut Class) -> Result<(), String> {
    let (declaration, obfuscated) = line
        .split_once(" -> ")
        .ok_or_else(|| "no `->` in a member line".to_owned())?;

    // `first:last:` source line numbers, present on methods and absent on
    // fields. Stripped by taking what follows the last colon that precedes the
    // type, rather than by counting colons: a return type never contains one.
    let declaration = match declaration.rsplit_once(':') {
        Some((_, rest)) => rest,
        None => declaration,
    };

    let (_type, rest) = declaration
        .split_once(' ')
        .ok_or_else(|| format!("no type in `{declaration}`"))?;

    match rest.split_once('(') {
        None => {
            class.fields.insert(rest.to_owned(), obfuscated.to_owned());
        }
        Some((name, arguments)) => {
            let arguments = arguments
                .strip_suffix(')')
                .ok_or_else(|| format!("unclosed parameter list in `{rest}`"))?;
            let parameters: Vec<String> = if arguments.is_empty() {
                Vec::new()
            } else {
                arguments.split(',').map(str::to_owned).collect()
            };
            class
                .methods
                .insert((name.to_owned(), parameters), obfuscated.to_owned());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fragment in the real file's shape. Written here rather than read from
    /// a fixture: the mappings are Mojang's and nothing of theirs is committed,
    /// which is the same rule the harness's own NBT reader is tested under.
    const SAMPLE: &str = "\
# (c) 2020 Microsoft Corporation. These mappings are provided \"as-is\".
net.minecraft.world.level.block.Block -> dfy:
# {\"fileName\":\"Block.java\",\"id\":\"sourceFile\"}
    net.minecraft.core.IdMapper BLOCK_STATE_REGISTRY -> q
    org.slf4j.Logger LOGGER -> a
    875:875:net.minecraft.world.level.block.Block getBlock() -> b
net.minecraft.world.level.block.state.BlockBehaviour$BlockStateBase -> dtb$a:
# {\"fileName\":\"BlockBehaviour.java\",\"id\":\"sourceFile\"}
    int lightEmission -> b
    918:918:boolean isValidSpawn(net.minecraft.world.level.BlockGetter,net.minecraft.core.BlockPos,net.minecraft.world.entity.EntityType) -> a
    922:925:boolean propagatesSkylightDown(net.minecraft.world.level.BlockGetter,net.minecraft.core.BlockPos) -> a
    929:932:int getLightBlock(net.minecraft.world.level.BlockGetter,net.minecraft.core.BlockPos) -> b
    957:957:int getLightEmission() -> h
";

    fn sample() -> Mappings {
        Mappings::parse(SAMPLE).expect("the sample parses")
    }

    #[test]
    fn classes_fields_and_methods_all_come_back() {
        let m = sample();
        assert_eq!(
            m.class("net.minecraft.world.level.block.Block"),
            Some("dfy")
        );
        assert_eq!(
            m.field(
                "net.minecraft.world.level.block.Block",
                "BLOCK_STATE_REGISTRY"
            ),
            Some("q")
        );
        assert_eq!(
            m.method(
                "net.minecraft.world.level.block.state.BlockBehaviour$BlockStateBase",
                "getLightEmission",
                &[]
            ),
            Some("h")
        );
    }

    #[test]
    fn an_inner_class_keeps_its_dollar_on_both_sides() {
        // `Class.forName` wants the `$` spelling, and so does the file. A
        // parser that split on it would produce a name Java cannot resolve.
        assert_eq!(
            sample().class("net.minecraft.world.level.block.state.BlockBehaviour$BlockStateBase"),
            Some("dtb$a")
        );
    }

    #[test]
    fn two_methods_of_one_class_may_share_an_obfuscated_name() {
        // The fact the whole keying decision rests on. `isValidSpawn` and
        // `propagatesSkylightDown` are both `a`; `getBlock` and `getLightBlock`
        // are both `b`. A table keyed by the obfuscated name would hold one of
        // each pair, and a table keyed by the source name alone would still
        // have to choose between overloads.
        let m = sample();
        let base = "net.minecraft.world.level.block.state.BlockBehaviour$BlockStateBase";
        let getter = "net.minecraft.world.level.BlockGetter";
        let pos = "net.minecraft.core.BlockPos";
        assert_eq!(
            m.method(base, "propagatesSkylightDown", &[getter, pos]),
            Some("a")
        );
        assert_eq!(
            m.method(
                base,
                "isValidSpawn",
                &[getter, pos, "net.minecraft.world.entity.EntityType"]
            ),
            Some("a")
        );
        assert_eq!(m.method(base, "getLightBlock", &[getter, pos]), Some("b"));
    }

    #[test]
    fn a_method_asked_for_with_the_wrong_parameters_is_absent_not_approximate() {
        // A near miss here is a call into a different method that happens to
        // share a letter, so the answer has to be `None` and not "the one with
        // that name".
        let m = sample();
        let base = "net.minecraft.world.level.block.state.BlockBehaviour$BlockStateBase";
        assert_eq!(m.method(base, "getLightBlock", &[]), None);
        assert_eq!(
            m.method(base, "getLightBlock", &["net.minecraft.core.BlockPos"]),
            None
        );
    }

    #[test]
    fn a_field_and_a_method_of_one_name_do_not_collide() {
        // `lightEmission` the field is `b` and `getLightEmission()` is `h`;
        // they live in separate maps because Java keeps them in separate
        // namespaces, and folding them together would make one shadow the
        // other depending on parse order.
        let m = sample();
        let base = "net.minecraft.world.level.block.state.BlockBehaviour$BlockStateBase";
        assert_eq!(m.field(base, "lightEmission"), Some("b"));
        assert_eq!(m.method(base, "getLightEmission", &[]), Some("h"));
    }

    #[test]
    fn comments_and_the_licence_header_are_not_members() {
        // The first line of the real file is a paragraph of English starting
        // with `#`, and every class is followed by a `# {"fileName": ...}`
        // line. Both are unindented-or-not and neither is data.
        assert_eq!(sample().len(), 2);
    }

    #[test]
    fn a_file_of_nothing_but_comments_parses_and_names_nothing() {
        // The condition `mappings_domain` guards on. It is not a parse error —
        // every line is legal — so without a check for it the failure surfaces
        // later as "twelve names missing", which sends a reader looking for a
        // renamed method in a file that has no methods in it.
        let m = Mappings::parse("# (c) 2020 Microsoft Corporation.\n# and another\n")
            .expect("comments are legal");
        assert!(m.is_empty());
        assert_eq!(m.len(), 0);
    }

    #[test]
    fn a_member_before_any_class_is_an_error_naming_its_line() {
        let why = Mappings::parse("    int lightEmission -> b\n").expect_err("refused");
        assert!(why.contains("line 1"), "{why}");
    }

    #[test]
    fn a_line_with_no_arrow_is_an_error_rather_than_a_skip() {
        // Skipping it would produce a table missing exactly the entries the
        // file was malformed around, and the oracle would fail later with a
        // message about the wrong thing.
        let why = Mappings::parse("this is not a mappings file\n").expect_err("refused");
        assert!(why.contains("no `->`"), "{why}");
    }

    #[test]
    fn a_properties_table_is_written_under_the_oracles_own_keys() {
        let m = sample();
        let base = "net.minecraft.world.level.block.state.BlockBehaviour$BlockStateBase";
        let getter = "net.minecraft.world.level.BlockGetter";
        let pos = "net.minecraft.core.BlockPos";
        let table = properties(
            &m,
            &[
                Wanted::Class {
                    key: "blockstate.class",
                    class: base,
                },
                Wanted::Field {
                    key: "blockstate.light_emission",
                    class: base,
                    field: "lightEmission",
                },
                Wanted::Method {
                    key: "blockstate.light_block",
                    class: base,
                    method: "getLightBlock",
                    parameters: &[getter, pos],
                },
            ],
        )
        .expect("all three resolve");
        assert!(table.contains("blockstate.class=dtb$a\n"), "{table}");
        assert!(table.contains("blockstate.light_emission=b\n"), "{table}");
        assert!(table.contains("blockstate.light_block=b\n"), "{table}");
        // And no Minecraft identifier is in the keys, which is what keeps the
        // Java side free of them.
        assert!(!table.contains("net.minecraft"), "{table}");
    }

    #[test]
    fn every_unresolved_entry_is_named_at_once() {
        // One list to work through after a version bump, not one failure per
        // run — and never a partial table, which would let the oracle start
        // and fail somewhere less obvious than here.
        let why = properties(
            &sample(),
            &[
                Wanted::Class {
                    key: "gone.class",
                    class: "net.minecraft.world.level.block.Nope",
                },
                Wanted::Field {
                    key: "gone.field",
                    class: "net.minecraft.world.level.block.Block",
                    field: "NOPE",
                },
            ],
        )
        .expect_err("neither resolves");
        assert!(why.contains("gone.class"), "{why}");
        assert!(why.contains("gone.field"), "{why}");
        assert!(why.contains("2 thing(s)"), "{why}");
    }
}

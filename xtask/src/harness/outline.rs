//! What shape is a registry, according to the server that sent it?
//!
//! # The problem this exists for
//!
//! `dust-server`'s `registries::schema` is a table of NBT types, one per key,
//! and its own module note says where those types have to come from: bytes a
//! real server sent, not a wiki and not the JSON on disk. JSON has one number
//! type where NBT has six, so the disk form cannot tell you whether
//! `"ambient_light": 0.0` is a float or a double. Reading the value guesses;
//! reading the wire does not.
//!
//! Ten registries were written that way by hand, from a capture read once and
//! then thrown away. That is fine for ten flat records and it does not scale
//! to the eleventh: `minecraft:enchantment` carries an open tree of effects,
//! several levels deep, different in every entry, and nobody is going to hold
//! forty-two of those in their head accurately.
//!
//! So this takes the entries as they arrived and reports the shape they have
//! in common — every key path, the tag type it holds, and how many entries
//! carry it. That is the schema table, derived rather than remembered.
//!
//! # What it reports and what it refuses to decide
//!
//! Three facts per path, all of them countable:
//!
//! * **The tag type.** More than one means a union, and the outline prints
//!   every type it saw rather than the most common one. `dimension_type`'s
//!   `monster_spawn_light_level` is an int in two dimensions and a compound in
//!   the other two, and a reader that saw only `TAG_Int` would write a schema
//!   that fails on the nether.
//! * **How many of its parent's observations carried it.** All of them means
//!   required; fewer means optional. The denominator is the *parent's* count
//!   and not the registry's, because a key three levels down inside an
//!   optional object is required whenever that object is present, and against
//!   the registry total it would read as rare.
//! * **How many distinct keys the parent had.** A record has a small fixed
//!   set that every observation repeats; a map has many keys that each appear
//!   once or twice. The outline prints the evidence and does not choose:
//!   `trim_material`'s `override_armor_materials` is a map with one key today,
//!   which no count could distinguish from a record.
//!
//! Lists are walked into and their elements folded together under a `[]`
//! segment, so a list of compounds reports the union of its elements' keys
//! with the counts that say which are always there. An empty list contributes
//! nothing but its own type, which is the honest answer: NBT lists are typed
//! and an empty one names no element type.

use std::collections::BTreeMap;

use super::nbt::Node;

/// Everything seen at one key path across a registry's entries.
#[derive(Debug, Default, Clone)]
pub struct Shape {
    /// How many observations of the parent carried this path at all.
    pub present: usize,
    /// How many observations the parent itself had — the denominator that
    /// makes `present` mean required or optional.
    pub of: usize,
    /// Every tag type seen here, with a count each. More than one is a union.
    pub types: BTreeMap<&'static str, usize>,
    /// For a compound: how many distinct keys were ever seen directly under
    /// it. Large against `present` is the signature of a map.
    pub keys: usize,
}

impl Shape {
    /// `true` when every observation of the parent carried this path.
    pub fn required(&self) -> bool {
        self.present == self.of && self.of > 0
    }

    /// The tag types seen, joined for printing. Sorted, so a union reads the
    /// same way on every run.
    pub fn type_names(&self) -> String {
        self.types.keys().copied().collect::<Vec<_>>().join(" | ")
    }
}

/// The name NBT gives a tag, as the wire format numbers them.
fn tag_name(node: &Node) -> &'static str {
    match node {
        Node::Byte(_) => "TAG_Byte",
        Node::Short(_) => "TAG_Short",
        Node::Int(_) => "TAG_Int",
        Node::Long(_) => "TAG_Long",
        Node::Float(_) => "TAG_Float",
        Node::Double(_) => "TAG_Double",
        Node::ByteArray(_) => "TAG_Byte_Array",
        Node::String(_) => "TAG_String",
        Node::List(_) => "TAG_List",
        Node::Compound(_) => "TAG_Compound",
        Node::IntArray(_) => "TAG_Int_Array",
        Node::LongArray(_) => "TAG_Long_Array",
    }
}

/// The shape of every key path under a set of observed roots.
///
/// The roots are one registry's entries — the compounds that arrived, with the
/// entries that carried no data left out by the caller, since "the client
/// already has this" says nothing about the shape.
///
/// Paths are dotted, with `[]` for a step into a list's elements. The map is
/// ordered, so a parent always prints before its children.
pub fn of(roots: &[&Node]) -> BTreeMap<String, Shape> {
    let mut out = BTreeMap::new();
    walk(&mut out, "", roots);
    out
}

/// Fold one level: record what each observation holds, then recurse per key.
///
/// `observations` are the nodes found at `prefix` — one per parent that had
/// it. Everything below is counted against `observations.len()`, which is what
/// makes "required" mean "whenever its parent is present".
fn walk(out: &mut BTreeMap<String, Shape>, prefix: &str, observations: &[&Node]) {
    if observations.is_empty() {
        return;
    }

    // A compound's children, gathered before any of them is described, so
    // every child knows the same denominator.
    let mut children: BTreeMap<&str, Vec<&Node>> = BTreeMap::new();
    let mut elements: Vec<&Node> = Vec::new();
    for node in observations {
        match node {
            Node::Compound(entries) => {
                for (name, value) in entries {
                    children.entry(name.as_str()).or_default().push(value);
                }
            }
            Node::List(items) => elements.extend(items.iter()),
            _ => {}
        }
    }

    if let Some(shape) = out.get_mut(prefix) {
        shape.keys = children.len();
    }

    for (name, values) in children {
        let path = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}.{name}")
        };
        record(out, &path, &values, observations.len());
        walk(out, &path, &values);
    }

    if !elements.is_empty() {
        let path = format!("{prefix}[]");
        // A list's elements are counted against the number of elements, not
        // the number of lists: "every element has a `type`" is the claim
        // worth making, and counting against the lists would read a
        // three-element list as three times over.
        record(out, &path, &elements, elements.len());
        walk(out, &path, &elements);
    }
}

/// Add one path's observations to the outline.
fn record(out: &mut BTreeMap<String, Shape>, path: &str, values: &[&Node], of: usize) {
    let shape = out.entry(path.to_owned()).or_default();
    shape.present += values.len();
    shape.of += of;
    for value in values {
        *shape.types.entry(tag_name(value)).or_default() += 1;
    }
}

/// `true` when a compound's keys look like data rather than a record's fields.
///
/// The test is **whether any key is always there**. A record has fields every
/// observation repeats; a map has keys that come and go with the data, and if
/// nothing is required then there is no field to name. Two keys minimum, so a
/// compound seen once with one key is not called a map on that alone.
///
/// It is a hint and not a verdict, and the reason is
/// `trim_material.override_armor_materials`: a map that happens to carry one
/// key in every entry it appears in, which no count can tell from a record.
/// The outline prints the evidence beside the hint so a reader can overrule it.
fn map_shaped(outline: &BTreeMap<String, Shape>, path: &str, shape: &Shape) -> bool {
    if shape.keys < 2 {
        return false;
    }
    let prefix = format!("{path}.");
    !outline
        .iter()
        .filter(|(child, _)| {
            child.starts_with(&prefix) && !child[prefix.len()..].contains(['.', '['])
        })
        .any(|(_, child)| child.required())
}

/// Print an outline the way a schema author reads it: indented by depth, the
/// type, then the evidence for required-or-optional.
pub fn print(outline: &BTreeMap<String, Shape>) {
    let width = outline.keys().map(String::len).max().unwrap_or(0).min(56);
    for (path, shape) in outline {
        let depth = path.matches('.').count();
        let leaf = path.rsplit('.').next().unwrap_or(path);
        let indent = "  ".repeat(depth + 1);
        let label = format!("{indent}{leaf}");
        let note = if shape.required() {
            "required".to_owned()
        } else {
            format!("optional  {}/{}", shape.present, shape.of)
        };
        let map_hint = if map_shaped(outline, path, shape) {
            format!("   {} keys, none required — map-shaped", shape.keys)
        } else {
            String::new()
        };
        println!(
            "  {label:<width$}  {:<28}  {note}{map_hint}",
            shape.type_names(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compound(pairs: &[(&str, Node)]) -> Node {
        Node::Compound(
            pairs
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.clone()))
                .collect(),
        )
    }

    #[test]
    fn a_key_every_entry_carries_is_required_and_one_it_does_not_is_not() {
        let a = compound(&[("weight", Node::Int(1)), ("effects", Node::Int(0))]);
        let b = compound(&[("weight", Node::Int(2))]);
        let outline = of(&[&a, &b]);
        assert!(outline["weight"].required());
        assert!(!outline["effects"].required());
        assert_eq!((outline["effects"].present, outline["effects"].of), (1, 2));
    }

    #[test]
    fn a_key_with_two_tag_types_reports_both() {
        // The reason this module exists rather than a reader looking at one
        // entry: `monster_spawn_light_level` is an int in two dimensions and a
        // compound in the other two, and a schema written from either alone
        // fails on the rest.
        let flat = compound(&[("light", Node::Int(0))]);
        let ranged = compound(&[("light", compound(&[("min_inclusive", Node::Int(0))]))]);
        let outline = of(&[&flat, &ranged]);
        assert_eq!(outline["light"].type_names(), "TAG_Compound | TAG_Int");
        assert!(outline["light"].required());
    }

    #[test]
    fn a_key_inside_an_optional_object_counts_against_that_object() {
        // Not against the registry. `effects.amount` is required whenever
        // `effects` is there, and counting it against all three entries would
        // read as one-in-three and be written optional — a key the client
        // then finds missing on an entry that must have it.
        let with = compound(&[("effects", compound(&[("amount", Node::Float(1.0))]))]);
        let without = compound(&[("weight", Node::Int(1))]);
        let outline = of(&[&with, &without, &without]);
        assert!(!outline["effects"].required());
        assert!(outline["effects.amount"].required());
        assert_eq!(
            (
                outline["effects.amount"].present,
                outline["effects.amount"].of
            ),
            (1, 1)
        );
    }

    #[test]
    fn list_elements_fold_together_under_one_path() {
        let a = compound(&[(
            "slots",
            Node::List(vec![
                Node::String("mainhand".to_owned()),
                Node::String("offhand".to_owned()),
            ]),
        )]);
        let outline = of(&[&a]);
        assert_eq!(outline["slots"].type_names(), "TAG_List");
        assert_eq!(outline["slots[]"].type_names(), "TAG_String");
        assert_eq!(outline["slots[]"].present, 2);
    }

    #[test]
    fn an_empty_list_names_no_element_type() {
        // NBT lists are typed and an empty one carries no element, so there is
        // nothing honest to say about what would go in it. Saying nothing is
        // the point: inventing an element type here would put a guess into a
        // table whose whole purpose is not to guess.
        let a = compound(&[("slots", Node::List(vec![]))]);
        let outline = of(&[&a]);
        assert_eq!(outline["slots"].type_names(), "TAG_List");
        assert!(!outline.contains_key("slots[]"));
    }

    #[test]
    fn a_compound_reports_how_many_distinct_keys_it_ever_had() {
        // The evidence that separates a record from a map. Two entries whose
        // `effects` share no key at all give four distinct keys over two
        // observations; a record would give the same key set twice.
        let a = compound(&[(
            "effects",
            compound(&[("damage", Node::Int(1)), ("armor", Node::Int(2))]),
        )]);
        let b = compound(&[(
            "effects",
            compound(&[("speed", Node::Int(3)), ("luck", Node::Int(4))]),
        )]);
        let outline = of(&[&a, &b]);
        assert_eq!(outline["effects"].keys, 4);
        assert_eq!(outline["effects"].present, 2);

        let record_shaped = of(&[&a, &a]);
        assert_eq!(record_shaped["effects"].keys, 2);
        assert_eq!(record_shaped["effects"].present, 2);
    }

    #[test]
    fn a_record_is_not_called_a_map_and_a_map_is() {
        // The distinction is "is anything always here", not "are there many
        // keys". A compound seen once with two required fields has more keys
        // than observations and is still a record.
        let record = compound(&[(
            "effect",
            compound(&[
                ("type", Node::String("x".to_owned())),
                ("value", Node::Float(1.0)),
            ]),
        )]);
        let outline = of(&[&record]);
        assert!(!map_shaped(&outline, "effect", &outline["effect"]));

        let a = compound(&[("effects", compound(&[("damage", Node::Int(1))]))]);
        let b = compound(&[("effects", compound(&[("speed", Node::Int(2))]))]);
        let outline = of(&[&a, &b]);
        assert!(map_shaped(&outline, "effects", &outline["effects"]));
    }

    #[test]
    fn the_map_hint_looks_only_at_direct_children() {
        // A grandchild that is required says nothing about whether this
        // compound's own keys are fields: every map's values have a shape.
        let a = compound(&[(
            "effects",
            compound(&[(
                "damage",
                compound(&[("type", Node::String("x".to_owned()))]),
            )]),
        )]);
        let b = compound(&[(
            "effects",
            compound(&[("speed", compound(&[("type", Node::String("y".to_owned()))]))]),
        )]);
        let outline = of(&[&a, &b]);
        assert!(outline["effects.damage.type"].required());
        assert!(map_shaped(&outline, "effects", &outline["effects"]));
    }

    #[test]
    fn nothing_is_reported_for_no_entries() {
        assert!(of(&[]).is_empty());
    }
}

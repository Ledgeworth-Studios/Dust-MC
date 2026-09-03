//! Compiling a data pack's density functions into a graph Dust can evaluate.
//!
//! The input is the directory `[data] path` names — the one holding
//! `minecraft/`, which is `data/` inside a data pack. Read from it:
//!
//! * `worldgen/noise_settings/<dimension>.json`, whose `noise_router` names the
//!   six climate functions.
//! * `worldgen/density_function/**.json`, the graph they are made of.
//! * `worldgen/noise/**.json`, the amplitudes each noise is shaped by.
//!
//! Nothing is compiled in, so an operator's data pack that reshapes the
//! overworld reshapes Dust's biomes with it, and a version that moves a
//! constant moves Dust's copy on the same day.
//!
//! # Why this reads the directory itself
//!
//! `dust-data` is the data *pack* reader — packs, overlays, the order rule —
//! and `[data] path` is not a pack: it is one pack's `data/` directory, which
//! is what `dust_server::registries::load` already reads the same way. Two
//! readers of one **layout** is a line each; two readers of one **schema** is
//! the defect this project keeps finding, and there is only one reader of the
//! density-function schema anywhere in the tree.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::blended::{BlendedNoise, BlendedShape};
use super::density::{Graph, Node, Rarity, Spline, SplinePoint, SplineValue};
use super::perlin::{NoiseParameters, NormalNoise};
use super::rng::{Positional, Xoroshiro};

/// What went wrong, with the file and the place inside it.
#[derive(Debug)]
pub enum BuildError {
    Unreadable {
        path: PathBuf,
        detail: String,
    },
    Malformed {
        path: PathBuf,
        detail: String,
    },
    /// A density function that refers to itself, directly or through others.
    Cycle {
        name: String,
    },
    /// A type this evaluator does not implement.
    ///
    /// Refused rather than defaulted to zero. A missing density-function type
    /// silently answering 0.0 would generate a whole world that is wrong in a
    /// way no test could name.
    UnknownType {
        name: String,
        kind: String,
    },
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable { path, detail } => write!(f, "{}: {detail}", path.display()),
            Self::Malformed { path, detail } => write!(f, "{}: {detail}", path.display()),
            Self::Cycle { name } => write!(f, "{name} refers to itself"),
            Self::UnknownType { name, kind } => {
                write!(f, "{name}: unsupported density function type `{kind}`")
            }
        }
    }
}

impl std::error::Error for BuildError {}

/// The name vanilla hashes the old blended noise's stream from. It names no
/// file: there is no `worldgen/noise/terrain.json`, and the amplitudes come
/// from the node's own five numbers instead.
const TERRAIN_NOISE: &str = "minecraft:terrain";

/// The six climate functions, in the order a climate point holds them.
pub const CLIMATE_ROUTES: [&str; 6] = [
    "temperature",
    "vegetation",
    "continents",
    "erosion",
    "depth",
    "ridges",
];

/// A compiled graph and the six roots the climate is read from.
#[derive(Debug, Clone)]
pub struct ClimateGraph {
    pub graph: Graph,
    pub roots: [usize; 6],
}

/// The one positional factory a world has.
///
/// Every noise, every surface rule's die and every aquifer draws from this,
/// and it is a pure function of the seed — which is why two servers with the
/// same seed and the same data pack serve the same world.
pub fn positional_factory(seed: i64) -> Positional {
    Xoroshiro::from_seed(seed).fork_positional()
}

/// The shape of a dimension's noise, as its settings file states it.
///
/// Not one of these numbers is written here. `cell_width` and `cell_height`
/// are the interpolation lattice a column's terrain is lerped across, and a
/// pack that halves either one generates a different world at a different
/// price; that is the operator's call to make and not this file's.
#[derive(Debug, Clone)]
pub struct NoiseSettings {
    pub min_y: i32,
    pub height: i32,
    pub cell_width: i32,
    pub cell_height: i32,
    pub sea_level: i32,
    /// The block a solid cell gets, as `(name, properties)`.
    pub default_block: BlockSpec,
    /// The block a cell below the fluid level gets.
    pub default_fluid: BlockSpec,
}

/// A block name and the properties a settings file wrote beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSpec {
    pub name: String,
    pub properties: Vec<(String, String)>,
}

/// A dimension's noise router: the climate half, the density half, and the
/// settings both are read under.
#[derive(Debug, Clone)]
pub struct Router {
    pub graph: Graph,
    /// The six climate roots, in [`CLIMATE_ROUTES`] order.
    pub climate: [usize; 6],
    /// `final_density`: positive is the default block, and everything else is
    /// air or fluid.
    pub final_density: usize,
    /// `initial_density_without_jaggedness`, which a surface rule walks down
    /// to find the coarse height of a column. `None` when the pack has none:
    /// the condition that reads it then refuses the whole surface branch
    /// rather than answering a guess.
    pub initial_density: Option<usize>,
    pub settings: NoiseSettings,
    /// The dimension's surface rules, compiled into the same graph.
    pub surface: Option<crate::surface::Rules>,
}

/// Compile a dimension's whole noise router — climate and terrain — for one
/// seed.
///
/// One graph and not two. `shift_x` is under five of the six climate functions
/// *and* under the offset spline the terrain's height comes from; compiled
/// apart they would be two noises with two permutation tables, sampled twice
/// per point.
pub fn router(root: &Path, dimension: &str, seed: i64) -> Result<Router, BuildError> {
    let mut builder = Builder::new(root);
    let settings_path = builder.path("noise_settings", dimension);
    let settings = read_json(&settings_path)?;
    let table = router_object(&settings, &settings_path)?;

    let mut climate = [0usize; 6];
    for (slot, name) in climate.iter_mut().zip(CLIMATE_ROUTES) {
        *slot = builder.compile_route(&table, name, &settings_path)?;
    }
    let final_density = builder.compile_route(&table, "final_density", &settings_path)?;
    // Optional because a pack may not have one, and the surface rules are the
    // only thing that reads it.
    let initial_density = match table.get("initial_density_without_jaggedness") {
        Some(entry) => Some(builder.compile(&entry.clone(), &settings_path)?),
        None => None,
    };
    let shape = noise_settings(&settings, &settings_path)?;

    // Compiled before the graph is finished, so the noises a surface rule
    // names land in the same table the density functions' did and are read
    // once, built once and sampled once.
    let surface = match settings.get("surface_rule") {
        Some(rule) => Some(crate::surface::compile(
            rule,
            &settings_path,
            &shape,
            seed,
            |name| builder.noise(name) as u32,
        )?),
        None => None,
    };

    let graph = builder.finish(seed)?;
    Ok(Router {
        graph,
        climate,
        final_density,
        initial_density,
        settings: shape,
        surface,
    })
}

fn router_object(
    settings: &Value,
    path: &Path,
) -> Result<serde_json::Map<String, Value>, BuildError> {
    Ok(settings
        .get("noise_router")
        .and_then(Value::as_object)
        .ok_or_else(|| BuildError::Malformed {
            path: path.to_path_buf(),
            detail: "no noise_router object".to_owned(),
        })?
        .clone())
}

fn noise_settings(settings: &Value, path: &Path) -> Result<NoiseSettings, BuildError> {
    let complain = |detail: String| BuildError::Malformed {
        path: path.to_path_buf(),
        detail,
    };
    let noise = settings
        .get("noise")
        .and_then(Value::as_object)
        .ok_or_else(|| complain("no `noise` object".to_owned()))?;
    let field = |name: &str| -> Result<i32, BuildError> {
        noise
            .get(name)
            .and_then(Value::as_i64)
            .map(|value| value as i32)
            .ok_or_else(|| complain(format!("noise has no `{name}`")))
    };
    let block = |name: &str| -> Result<BlockSpec, BuildError> {
        let entry = settings
            .get(name)
            .and_then(Value::as_object)
            .ok_or_else(|| complain(format!("no `{name}` object")))?;
        let id = entry
            .get("Name")
            .and_then(Value::as_str)
            .ok_or_else(|| complain(format!("`{name}` has no `Name`")))?;
        let mut properties: Vec<(String, String)> = entry
            .get("Properties")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|value| (key.clone(), value.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        properties.sort();
        Ok(BlockSpec {
            name: namespaced(id),
            properties,
        })
    };
    Ok(NoiseSettings {
        min_y: field("min_y")?,
        height: field("height")?,
        // Quart positions, times four. A `size_horizontal` of 1 is a cell four
        // blocks wide, which is why a chunk is four cells across and not
        // sixteen.
        cell_width: field("size_horizontal")? * 4,
        cell_height: field("size_vertical")? * 4,
        sea_level: settings
            .get("sea_level")
            .and_then(Value::as_i64)
            .map(|value| value as i32)
            .ok_or_else(|| complain("no `sea_level`".to_owned()))?,
        default_block: block("default_block")?,
        default_fluid: block("default_fluid")?,
    })
}

/// Compile the climate half of a dimension's noise router for one world seed.
pub fn climate_graph(root: &Path, dimension: &str, seed: i64) -> Result<ClimateGraph, BuildError> {
    let mut builder = Builder::new(root);
    let settings_path = builder.path("noise_settings", dimension);
    let settings = read_json(&settings_path)?;
    let table = router_object(&settings, &settings_path)?;

    let mut roots = [0usize; 6];
    for (slot, name) in roots.iter_mut().zip(CLIMATE_ROUTES) {
        *slot = builder.compile_route(&table, name, &settings_path)?;
    }

    let graph = builder.finish(seed)?;
    Ok(ClimateGraph { graph, roots })
}

struct Builder {
    root: PathBuf,
    nodes: Vec<Node>,
    splines: Vec<Spline>,
    /// Density functions already compiled, by namespaced name.
    functions: HashMap<String, usize>,
    /// Names currently being compiled, so a cycle is an error rather than a
    /// stack overflow.
    in_progress: Vec<String>,
    /// Noise names in the order they were first reached, with their shapes.
    noise_names: Vec<String>,
    noise_index: HashMap<String, usize>,
    /// One entry per distinct `old_blended_noise` shape. Two nodes with the
    /// same shape are the same noise: the stream is drawn from the same hash
    /// of the same name, so building it twice would build it identically.
    blended: Vec<BlendedShape>,
    /// The argument of each `interpolated` node, indexed by its slot.
    interpolated: Vec<usize>,
}

impl Builder {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            nodes: Vec::new(),
            splines: Vec::new(),
            functions: HashMap::new(),
            in_progress: Vec::new(),
            noise_names: Vec::new(),
            noise_index: HashMap::new(),
            blended: Vec::new(),
            interpolated: Vec::new(),
        }
    }

    /// Compile one entry of a noise router by name.
    fn compile_route(
        &mut self,
        table: &serde_json::Map<String, Value>,
        name: &str,
        origin: &Path,
    ) -> Result<usize, BuildError> {
        let entry = table.get(name).ok_or_else(|| BuildError::Malformed {
            path: origin.to_path_buf(),
            detail: format!("the noise router has no `{name}`"),
        })?;
        self.compile(&entry.clone(), origin)
    }

    /// Seed everything the graph named and hand the graph over.
    fn finish(self, seed: i64) -> Result<Graph, BuildError> {
        // One factory for the whole world, forked from the seed exactly once,
        // and every noise drawn from it by name.
        let factory = positional_factory(seed);
        let noises = self.build_noises(&factory)?;
        let blended = self
            .blended
            .iter()
            .map(|shape| {
                // Vanilla hands the old blended noise the stream hashed from
                // `minecraft:terrain` — the one name in the router that names
                // no file.
                let mut stream = factory.from_hash_of(TERRAIN_NOISE);
                BlendedNoise::new(&mut stream, *shape)
            })
            .collect();
        Ok(Graph {
            nodes: self.nodes,
            splines: self.splines,
            noises,
            blended,
            interpolated: self.interpolated,
        })
    }

    /// `<root>/minecraft/worldgen/<registry>/<path>.json`, from a namespaced
    /// name.
    fn path(&self, registry: &str, name: &str) -> PathBuf {
        let (namespace, path) = split_namespace(name);
        self.root
            .join(namespace)
            .join("worldgen")
            .join(registry)
            .join(format!("{path}.json"))
    }

    fn push(&mut self, node: Node) -> usize {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    /// Give a noise name an index, reading nothing yet — the shapes are read
    /// once, after the graph is complete, so a noise reached from four places
    /// is read once and built once.
    ///
    /// The name is namespaced first. A noise is seeded from the **string** of
    /// its resource location, and Minecraft's own `ResourceLocation` carries
    /// the namespace whether or not the JSON wrote one: a pack that says
    /// `"noise": "temperature"` means `minecraft:temperature` and must get
    /// `minecraft:temperature`'s stream, not a stream nothing else has.
    fn noise(&mut self, name: &str) -> usize {
        let name = namespaced(name);
        if let Some(&index) = self.noise_index.get(&name) {
            return index;
        }
        let index = self.noise_names.len();
        self.noise_names.push(name.clone());
        self.noise_index.insert(name, index);
        index
    }

    /// Every noise is drawn by *name*, which is why building five of vanilla's
    /// noises and not the other fifty-five still gives the five Minecraft
    /// would have given.
    fn build_noises(&self, factory: &Positional) -> Result<Vec<NormalNoise>, BuildError> {
        let mut built = Vec::with_capacity(self.noise_names.len());
        for name in &self.noise_names {
            let path = self.path("noise", name);
            let value = read_json(&path)?;
            let parameters = noise_parameters(&value, &path)?;
            let mut stream = factory.from_hash_of(name);
            built.push(NormalNoise::create(&mut stream, &parameters));
        }
        Ok(built)
    }

    /// Compile one JSON value into a node index.
    ///
    /// `origin` is only used to say which file a complaint is about.
    fn compile(&mut self, value: &Value, origin: &Path) -> Result<usize, BuildError> {
        if let Some(number) = value.as_f64() {
            return Ok(self.push(Node::Constant(number)));
        }
        if let Some(name) = value.as_str() {
            return self.compile_reference(name);
        }
        let object = value.as_object().ok_or_else(|| BuildError::Malformed {
            path: origin.to_path_buf(),
            detail: "a density function is a number, a name or an object".to_owned(),
        })?;
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| BuildError::Malformed {
                path: origin.to_path_buf(),
                detail: "a density function object needs a `type`".to_owned(),
            })?
            .to_owned();

        let node = match kind.as_str() {
            "minecraft:abs" => {
                let argument = self.child(object, "argument", origin)?;
                Node::Abs(argument)
            }
            "minecraft:add" | "minecraft:mul" | "minecraft:min" | "minecraft:max" => {
                let a = self.child(object, "argument1", origin)?;
                let b = self.child(object, "argument2", origin)?;
                match kind.as_str() {
                    "minecraft:add" => Node::Add(a, b),
                    "minecraft:mul" => Node::Mul(a, b),
                    "minecraft:min" => Node::Min(a, b),
                    _ => Node::Max(a, b),
                }
            }
            "minecraft:blend_alpha" => Node::BlendAlpha,
            "minecraft:blend_offset" => Node::BlendOffset,
            // `blend_density` is old-chunk blending, which a world with no
            // old chunks in it leaves alone.
            "minecraft:cache_once" | "minecraft:blend_density" => {
                let argument = self.child(object, "argument", origin)?;
                Node::Passthrough(argument)
            }
            "minecraft:cache_2d" => {
                let argument = self.child(object, "argument", origin)?;
                Node::ColumnCache(argument)
            }
            "minecraft:flat_cache" => {
                let argument = self.child(object, "argument", origin)?;
                Node::FlatCache(argument)
            }
            "minecraft:interpolated" => {
                let argument = self.child(object, "argument", origin)?;
                self.interpolated.push(argument);
                Node::Interpolated(self.interpolated.len() - 1)
            }
            "minecraft:square" => Node::Square(self.child(object, "argument", origin)?),
            "minecraft:cube" => Node::Cube(self.child(object, "argument", origin)?),
            "minecraft:half_negative" => {
                Node::HalfNegative(self.child(object, "argument", origin)?)
            }
            "minecraft:quarter_negative" => {
                Node::QuarterNegative(self.child(object, "argument", origin)?)
            }
            "minecraft:squeeze" => Node::Squeeze(self.child(object, "argument", origin)?),
            "minecraft:clamp" => Node::Clamp {
                argument: self.child(object, "input", origin)?,
                min: number(object, "min", origin)?,
                max: number(object, "max", origin)?,
            },
            "minecraft:range_choice" => Node::RangeChoice {
                input: self.child(object, "input", origin)?,
                min_inclusive: number(object, "min_inclusive", origin)?,
                max_exclusive: number(object, "max_exclusive", origin)?,
                in_range: self.child(object, "when_in_range", origin)?,
                out_of_range: self.child(object, "when_out_of_range", origin)?,
            },
            "minecraft:weird_scaled_sampler" => {
                let name = self.noise_name(object, origin)?;
                let noise = self.noise(&name);
                let mapper = object
                    .get("rarity_value_mapper")
                    .and_then(Value::as_str)
                    .ok_or_else(|| BuildError::Malformed {
                        path: origin.to_path_buf(),
                        detail: "missing `rarity_value_mapper`".to_owned(),
                    })?;
                let rarity = match mapper {
                    "type_1" => Rarity::Type1,
                    "type_2" => Rarity::Type2,
                    // Refused rather than defaulted. The two mappers differ by
                    // a factor of one and a half at the same input, which is
                    // the difference between a cave and solid rock.
                    other => {
                        return Err(BuildError::UnknownType {
                            name: origin.display().to_string(),
                            kind: format!("rarity_value_mapper `{other}`"),
                        })
                    }
                };
                Node::WeirdScaledSampler {
                    input: self.child(object, "input", origin)?,
                    noise,
                    rarity,
                }
            }
            "minecraft:old_blended_noise" => {
                let shape = BlendedShape {
                    xz_scale: number(object, "xz_scale", origin)?,
                    y_scale: number(object, "y_scale", origin)?,
                    xz_factor: number(object, "xz_factor", origin)?,
                    y_factor: number(object, "y_factor", origin)?,
                    smear_scale_multiplier: number(object, "smear_scale_multiplier", origin)?,
                };
                let index = match self.blended.iter().position(|held| *held == shape) {
                    Some(index) => index,
                    None => {
                        self.blended.push(shape);
                        self.blended.len() - 1
                    }
                };
                Node::Blended(index)
            }
            "minecraft:shift_a" | "minecraft:shift_b" => {
                let name = object
                    .get("argument")
                    .and_then(Value::as_str)
                    .ok_or_else(|| BuildError::Malformed {
                        path: origin.to_path_buf(),
                        detail: format!("{kind} needs a noise name in `argument`"),
                    })?
                    .to_owned();
                let noise = self.noise(&name);
                if kind == "minecraft:shift_a" {
                    Node::ShiftA(noise)
                } else {
                    Node::ShiftB(noise)
                }
            }
            "minecraft:noise" => {
                let name = self.noise_name(object, origin)?;
                let noise = self.noise(&name);
                Node::Noise {
                    noise,
                    xz_scale: number(object, "xz_scale", origin)?,
                    y_scale: number(object, "y_scale", origin)?,
                }
            }
            "minecraft:shifted_noise" => {
                let name = self.noise_name(object, origin)?;
                let noise = self.noise(&name);
                let shift_x = self.child(object, "shift_x", origin)?;
                let shift_y = self.child(object, "shift_y", origin)?;
                let shift_z = self.child(object, "shift_z", origin)?;
                Node::ShiftedNoise {
                    noise,
                    shift_x,
                    shift_y,
                    shift_z,
                    xz_scale: number(object, "xz_scale", origin)?,
                    y_scale: number(object, "y_scale", origin)?,
                }
            }
            "minecraft:spline" => {
                let spline = object.get("spline").ok_or_else(|| BuildError::Malformed {
                    path: origin.to_path_buf(),
                    detail: "a spline function needs a `spline`".to_owned(),
                })?;
                let index = self.compile_spline(&spline.clone(), origin)?;
                Node::Spline(index)
            }
            "minecraft:y_clamped_gradient" => Node::YClampedGradient {
                from_y: number(object, "from_y", origin)?,
                to_y: number(object, "to_y", origin)?,
                from_value: number(object, "from_value", origin)?,
                to_value: number(object, "to_value", origin)?,
            },
            other => {
                return Err(BuildError::UnknownType {
                    name: origin.display().to_string(),
                    kind: other.to_owned(),
                })
            }
        };
        Ok(self.push(node))
    }

    fn compile_reference(&mut self, name: &str) -> Result<usize, BuildError> {
        if let Some(&index) = self.functions.get(name) {
            return Ok(index);
        }
        if self.in_progress.iter().any(|open| open == name) {
            return Err(BuildError::Cycle {
                name: name.to_owned(),
            });
        }
        let path = self.path("density_function", name);
        let value = read_json(&path)?;
        self.in_progress.push(name.to_owned());
        let compiled = self.compile(&value, &path);
        self.in_progress.pop();
        let index = compiled?;
        self.functions.insert(name.to_owned(), index);
        Ok(index)
    }

    fn child(
        &mut self,
        object: &serde_json::Map<String, Value>,
        key: &str,
        origin: &Path,
    ) -> Result<usize, BuildError> {
        let value = object.get(key).ok_or_else(|| BuildError::Malformed {
            path: origin.to_path_buf(),
            detail: format!("missing `{key}`"),
        })?;
        self.compile(&value.clone(), origin)
    }

    fn noise_name(
        &self,
        object: &serde_json::Map<String, Value>,
        origin: &Path,
    ) -> Result<String, BuildError> {
        object
            .get("noise")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| BuildError::Malformed {
                path: origin.to_path_buf(),
                detail: "missing a noise name in `noise`".to_owned(),
            })
    }

    fn compile_spline(&mut self, value: &Value, origin: &Path) -> Result<usize, BuildError> {
        let object = value.as_object().ok_or_else(|| BuildError::Malformed {
            path: origin.to_path_buf(),
            detail: "a spline is an object".to_owned(),
        })?;
        let coordinate_name = object
            .get("coordinate")
            .and_then(Value::as_str)
            .ok_or_else(|| BuildError::Malformed {
                path: origin.to_path_buf(),
                detail: "a spline needs a `coordinate`".to_owned(),
            })?
            .to_owned();
        let coordinate = self.compile_reference(&coordinate_name)?;
        let raw = object
            .get("points")
            .and_then(Value::as_array)
            .ok_or_else(|| BuildError::Malformed {
                path: origin.to_path_buf(),
                detail: "a spline needs `points`".to_owned(),
            })?
            .clone();
        let mut points = Vec::with_capacity(raw.len());
        for point in &raw {
            let point = point.as_object().ok_or_else(|| BuildError::Malformed {
                path: origin.to_path_buf(),
                detail: "a spline point is an object".to_owned(),
            })?;
            let location = number(point, "location", origin)? as f32;
            let derivative = number(point, "derivative", origin)? as f32;
            let value = point.get("value").ok_or_else(|| BuildError::Malformed {
                path: origin.to_path_buf(),
                detail: "a spline point needs a `value`".to_owned(),
            })?;
            let value = if let Some(constant) = value.as_f64() {
                SplineValue::Constant(constant as f32)
            } else {
                SplineValue::Nested(self.compile_spline(&value.clone(), origin)?)
            };
            points.push(SplinePoint {
                location,
                value,
                derivative,
            });
        }
        self.splines.push(Spline { coordinate, points });
        Ok(self.splines.len() - 1)
    }
}

/// A resource location's two halves, with the namespace vanilla assumes when
/// one is not written.
fn split_namespace(name: &str) -> (&str, &str) {
    match name.split_once(':') {
        Some((namespace, path)) => (namespace, path),
        None => ("minecraft", name),
    }
}

/// The same name with its namespace written out, which is the spelling
/// Minecraft hashes.
fn namespaced(name: &str) -> String {
    let (namespace, path) = split_namespace(name);
    format!("{namespace}:{path}")
}

fn number(
    object: &serde_json::Map<String, Value>,
    key: &str,
    origin: &Path,
) -> Result<f64, BuildError> {
    object
        .get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| BuildError::Malformed {
            path: origin.to_path_buf(),
            detail: format!("`{key}` is missing or is not a number"),
        })
}

fn noise_parameters(value: &Value, path: &Path) -> Result<NoiseParameters, BuildError> {
    let object = value.as_object().ok_or_else(|| BuildError::Malformed {
        path: path.to_path_buf(),
        detail: "noise parameters are an object".to_owned(),
    })?;
    let first_octave = object
        .get("firstOctave")
        .and_then(Value::as_i64)
        .ok_or_else(|| BuildError::Malformed {
            path: path.to_path_buf(),
            detail: "no `firstOctave`".to_owned(),
        })? as i32;
    let amplitudes = object
        .get("amplitudes")
        .and_then(Value::as_array)
        .ok_or_else(|| BuildError::Malformed {
            path: path.to_path_buf(),
            detail: "no `amplitudes`".to_owned(),
        })?
        .iter()
        .map(|entry| {
            entry.as_f64().ok_or_else(|| BuildError::Malformed {
                path: path.to_path_buf(),
                detail: "an amplitude is not a number".to_owned(),
            })
        })
        .collect::<Result<Vec<f64>, BuildError>>()?;
    Ok(NoiseParameters {
        first_octave,
        amplitudes,
    })
}

fn read_json(path: &Path) -> Result<Value, BuildError> {
    let bytes = std::fs::read(path).map_err(|e| BuildError::Unreadable {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })?;
    serde_json::from_slice(&bytes).map_err(|e| BuildError::Malformed {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::density::Evaluator;

    /// A minimal data pack on disk. Written by the test, so nothing of
    /// Mojang's is needed to check that the reader reads.
    struct Pack {
        dir: PathBuf,
    }

    impl Pack {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("dust-gen-build-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Self { dir }
        }

        fn write(&self, registry: &str, name: &str, json: &str) {
            let path = self
                .dir
                .join("minecraft")
                .join("worldgen")
                .join(registry)
                .join(format!("{name}.json"));
            std::fs::create_dir_all(path.parent().expect("has a parent")).expect("made the tree");
            std::fs::write(path, json).expect("wrote the file");
        }
    }

    impl Drop for Pack {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn settings(router: &str) -> String {
        format!("{{\"noise_router\": {router}}}")
    }

    #[test]
    fn a_router_of_constants_compiles_and_evaluates() {
        let pack = Pack::new("constants");
        pack.write(
            "noise_settings",
            "test",
            &settings(
                r#"{"temperature": 0.5, "vegetation": -0.25, "continents": 0.0,
                    "erosion": 1.0, "depth": {"type": "minecraft:y_clamped_gradient",
                    "from_y": 0, "to_y": 100, "from_value": 0.0, "to_value": 1.0},
                    "ridges": {"type": "minecraft:abs", "argument": -2.0}}"#,
            ),
        );
        let built = climate_graph(&pack.dir, "test", 0).expect("compiled");
        let mut evaluator = Evaluator::new(&built.graph);
        let mut out = [0.0; 6];
        evaluator.compute_all(&built.roots, 0, 50, 0, &mut out);
        assert_eq!(out[0], 0.5);
        assert_eq!(out[1], -0.25);
        assert_eq!(out[3], 1.0);
        assert_eq!(out[4], 0.5, "the gradient is read at y");
        assert_eq!(out[5], 2.0);
    }

    #[test]
    fn a_named_function_is_read_from_the_pack_and_shared() {
        let pack = Pack::new("shared");
        pack.write("density_function", "half", "0.5");
        pack.write(
            "noise_settings",
            "test",
            &settings(
                r#"{"temperature": "minecraft:half", "vegetation": "minecraft:half",
                    "continents": 0.0, "erosion": 0.0, "depth": 0.0, "ridges": 0.0}"#,
            ),
        );
        let built = climate_graph(&pack.dir, "test", 0).expect("compiled");
        assert_eq!(
            built.roots[0], built.roots[1],
            "one name is one node, not two"
        );
    }

    #[test]
    fn a_function_that_refers_to_itself_is_refused_rather_than_recursed() {
        let pack = Pack::new("cycle");
        pack.write(
            "density_function",
            "loop",
            r#"{"type": "minecraft:abs", "argument": "minecraft:loop"}"#,
        );
        pack.write(
            "noise_settings",
            "test",
            &settings(
                r#"{"temperature": "minecraft:loop", "vegetation": 0.0, "continents": 0.0,
                    "erosion": 0.0, "depth": 0.0, "ridges": 0.0}"#,
            ),
        );
        assert!(matches!(
            climate_graph(&pack.dir, "test", 0),
            Err(BuildError::Cycle { .. })
        ));
    }

    #[test]
    fn a_type_this_evaluator_does_not_know_is_refused_and_not_defaulted() {
        // Watched to fail: returning `Node::Constant(0.0)` for an unknown type
        // makes this test pass a graph that generates a wrong world in silence.
        let pack = Pack::new("unknown");
        pack.write(
            "noise_settings",
            "test",
            &settings(
                r#"{"temperature": {"type": "minecraft:end_islands"}, "vegetation": 0.0,
                    "continents": 0.0, "erosion": 0.0, "depth": 0.0, "ridges": 0.0}"#,
            ),
        );
        assert!(matches!(
            climate_graph(&pack.dir, "test", 0),
            Err(BuildError::UnknownType { .. })
        ));
    }

    #[test]
    fn a_noise_is_read_once_however_many_places_reach_it() {
        let pack = Pack::new("noises");
        pack.write(
            "noise",
            "shared",
            r#"{"firstOctave": -7, "amplitudes": [1.0, 1.0]}"#,
        );
        pack.write(
            "noise",
            "offset",
            r#"{"firstOctave": -3, "amplitudes": [1.0, 1.0, 1.0, 0.0]}"#,
        );
        pack.write(
            "density_function",
            "shift_x",
            r#"{"type": "minecraft:shift_a", "argument": "minecraft:offset"}"#,
        );
        pack.write(
            "noise_settings",
            "test",
            &settings(
                r#"{"temperature": {"type": "minecraft:shifted_noise", "noise": "minecraft:shared",
                      "shift_x": "minecraft:shift_x", "shift_y": 0.0, "shift_z": 0.0,
                      "xz_scale": 0.25, "y_scale": 0.0},
                    "vegetation": {"type": "minecraft:shifted_noise", "noise": "minecraft:shared",
                      "shift_x": "minecraft:shift_x", "shift_y": 0.0, "shift_z": 0.0,
                      "xz_scale": 0.25, "y_scale": 0.0},
                    "continents": 0.0, "erosion": 0.0, "depth": 0.0, "ridges": 0.0}"#,
            ),
        );
        let built = climate_graph(&pack.dir, "test", 12345).expect("compiled");
        assert_eq!(built.graph.noises.len(), 2, "shared and offset, once each");
        let mut evaluator = Evaluator::new(&built.graph);
        let mut out = [0.0; 6];
        evaluator.compute_all(&built.roots, 40, 0, -24, &mut out);
        assert_eq!(out[0], out[1]);
        assert_ne!(out[0], 0.0, "a real noise is not flat");
    }

    #[test]
    fn a_seed_changes_the_world_and_the_same_seed_does_not() {
        let pack = Pack::new("seeded");
        pack.write(
            "noise",
            "shared",
            r#"{"firstOctave": -7, "amplitudes": [1.0, 1.0]}"#,
        );
        pack.write(
            "noise_settings",
            "test",
            &settings(
                r#"{"temperature": {"type": "minecraft:noise", "noise": "minecraft:shared",
                      "xz_scale": 0.25, "y_scale": 0.0},
                    "vegetation": 0.0, "continents": 0.0, "erosion": 0.0, "depth": 0.0,
                    "ridges": 0.0}"#,
            ),
        );
        let sample = |seed| {
            let built = climate_graph(&pack.dir, "test", seed).expect("compiled");
            let mut evaluator = Evaluator::new(&built.graph);
            let mut out = [0.0; 6];
            evaluator.compute_all(&built.roots, 100, 0, 100, &mut out);
            out[0]
        };
        assert_eq!(sample(1), sample(1));
        assert_ne!(sample(1), sample(2));
    }
}

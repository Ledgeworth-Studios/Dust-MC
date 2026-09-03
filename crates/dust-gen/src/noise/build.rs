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

use super::density::{Graph, Node, Spline, SplinePoint, SplineValue};
use super::perlin::{NoiseParameters, NormalNoise};
use super::rng::Xoroshiro;

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

/// Compile the climate half of a dimension's noise router for one world seed.
pub fn climate_graph(root: &Path, dimension: &str, seed: i64) -> Result<ClimateGraph, BuildError> {
    let mut builder = Builder::new(root);
    let settings_path = builder.path("noise_settings", dimension);
    let settings = read_json(&settings_path)?;
    let router = settings
        .get("noise_router")
        .and_then(Value::as_object)
        .ok_or_else(|| BuildError::Malformed {
            path: settings_path.clone(),
            detail: "no noise_router object".to_owned(),
        })?
        .clone();

    let mut roots = [0usize; 6];
    for (slot, name) in roots.iter_mut().zip(CLIMATE_ROUTES) {
        let entry = router.get(name).ok_or_else(|| BuildError::Malformed {
            path: settings_path.clone(),
            detail: format!("the noise router has no `{name}`"),
        })?;
        *slot = builder.compile(entry, &settings_path)?;
    }

    let noises = builder.build_noises(seed)?;
    Ok(ClimateGraph {
        graph: Graph {
            nodes: builder.nodes,
            splines: builder.splines,
            noises,
        },
        roots,
    })
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
        }
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

    fn build_noises(&self, seed: i64) -> Result<Vec<NormalNoise>, BuildError> {
        // One factory for the whole world, forked from the seed exactly once.
        // Every noise is then drawn by *name*, which is why building five of
        // vanilla's noises and not the other fifty-five still gives the five
        // Minecraft would have given.
        let mut base = Xoroshiro::from_seed(seed);
        let factory = base.fork_positional();
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
            "minecraft:cache_once" | "minecraft:interpolated" | "minecraft:blend_density" => {
                let argument = self.child(object, "argument", origin)?;
                Node::Passthrough(argument)
            }
            "minecraft:flat_cache" | "minecraft:cache_2d" => {
                let argument = self.child(object, "argument", origin)?;
                Node::ColumnCache(argument)
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

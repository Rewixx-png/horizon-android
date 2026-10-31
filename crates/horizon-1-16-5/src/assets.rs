//! 1.16.5 resource-pack format 6 loading and model baking.
//!
//! `ModelPack::load` reads the `blockstates/` and `models/` folders of an
//! `assets/minecraft` directory and resolves every model's parent chain and
//! texture variables up front. The [`ModelSource`](crate::ModelSource)
//! implementation then selects a blockstate variant (or every matching
//! multipart) for a state and bakes it into quads in model space (0..16),
//! mirroring vanilla's `ModelBlockDefinition` selection, `BlockModel` baking
//! and `FaceBakery.bakeQuad` geometry, including the x/y quarter-turn block
//! rotation, face UV locking and element rotations.
//!
//! Quads carry texture *names* only; sprite lookup is outside this crate.
//! Baked models are memoized per block state inside the pack, so selecting
//! the same state repeatedly returns an equal clone instead of re-baking.
//! Weighted variant lists pick their first entry deterministically (vanilla
//! would roll the listed weights), and a texture variable that cannot be
//! resolved becomes `minecraft:missingno`, as vanilla falls back to its
//! missing sprite.

use crate::{BakedModel, BlockState, Face, ModelSource, Quad, Vertex};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::Path;

/// The format-6 model assets of one resource pack, loaded from an
/// `assets/minecraft` directory.
#[derive(Debug)]
pub struct ModelPack {
    blockstates: HashMap<String, BlockstateDef>,
    models: HashMap<String, Option<ResolvedModel>>,
    /// Baked results per state, `None` included, so a chunk mesher that asks
    /// for the same state once per block does not re-bake its geometry.
    /// Interior mutability keeps [`ModelSource::model`] a `&self` operation.
    cache: RefCell<HashMap<BlockState, Option<BakedModel>>>,
}

/// One parsed blockstate definition: either an ordered variant list or a
/// multipart list. Vanilla rejects files that carry both, as does this loader.
#[derive(Debug)]
struct BlockstateDef {
    variants: Vec<VariantEntry>,
    multipart: Vec<MultipartPart>,
}

/// A variant key (possibly empty for the default) plus the models it applies.
#[derive(Debug)]
struct VariantEntry {
    key: String,
    apply: Apply,
}

/// A multipart part: an optional condition and the models it applies.
#[derive(Debug)]
struct MultipartPart {
    when: Option<Condition>,
    apply: Apply,
}

/// A weighted variant list. Baking picks the first entry deterministically.
#[derive(Debug)]
struct Apply {
    entries: Vec<Variant>,
}

/// One variant application: a model plus its block rotation.
#[derive(Clone, Debug)]
struct Variant {
    model: String,
    x: u8,
    y: u8,
    uvlock: bool,
}

/// A multipart condition, mirroring vanilla's `ICondition` tree.
#[derive(Debug)]
enum Condition {
    Key {
        key: String,
        negated: bool,
        values: Vec<String>,
    },
    Or(Vec<Condition>),
    And(Vec<Condition>),
}

impl Condition {
    fn matches(&self, state: &BlockState) -> bool {
        match self {
            Self::Key {
                key,
                negated,
                values,
            } => {
                let hit = state
                    .properties
                    .get(key)
                    .is_some_and(|value| values.iter().any(|candidate| candidate == value));
                hit != *negated
            }
            Self::Or(conditions) => conditions.iter().any(|condition| condition.matches(state)),
            Self::And(conditions) => conditions.iter().all(|condition| condition.matches(state)),
        }
    }
}

/// A model file before parent resolution: faces still reference `#variables`.
#[derive(Debug)]
struct RawModel {
    parent: Option<String>,
    textures: HashMap<String, String>,
    elements: Vec<Element>,
    /// Whether the file declared an `elements` member (even `null` counts as
    /// absent). A model that declares its own elements replaces the geometry
    /// of every ancestor, exactly as vanilla bakes only its own `elements`.
    has_elements: bool,
}

/// A model whose parent chain, texture merge and variable resolution are done.
#[derive(Debug)]
struct ResolvedModel {
    elements: Vec<Element>,
    /// The merged texture variables resolved to absolute sprite names, e.g.
    /// `layer0 -> minecraft:item/apple`. Element-less models such as
    /// `item/generated` carry no quads, so their layers are reported from
    /// this map instead.
    textures: HashMap<String, String>,
}

/// One item's 3D inventory render: the baked quads plus the sprite names they
/// (or, for a generated item model, their texture layers) use.
///
/// Quads are in model space (0..16); nothing here is rasterized. A caller
/// that wants a tiny flat icon reads the sprite PNGs instead, as
/// [`crate::item_icons::ItemIcons`] does.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ItemRender {
    pub model: BakedModel,
    /// Distinct texture names in sorted order, so the list is deterministic.
    pub textures: Vec<String>,
}

/// One cuboid element in 0..16 model space.
#[derive(Clone, Debug)]
struct Element {
    from: [f32; 3],
    to: [f32; 3],
    rotation: Option<ElementRotation>,
    faces: [Option<RawFace>; 6],
}

/// An element rotation around its origin, in 22.5-degree steps.
#[derive(Clone, Copy, Debug)]
struct ElementRotation {
    origin: [f32; 3],
    axis: Axis,
    angle: f32,
    rescale: bool,
}

#[derive(Clone, Copy, Debug)]
enum Axis {
    X,
    Y,
    Z,
}

/// One element face. `texture` is a `#variable` until resolution.
#[derive(Clone, Debug)]
struct RawFace {
    uv: Option<[f32; 4]>,
    texture: String,
    cullface: Option<Face>,
    /// Quarter turns: 0..3.
    rotation: u8,
}

/// The `minecraft:missingno` fallback for unresolvable texture variables.
const MISSING_TEXTURE: &str = "minecraft:missingno";

/// The raw JSON of one format-6 model pack, before parent resolution.
/// Paths are relative to the pack's `assets/minecraft` folder and use `/`,
/// e.g. `blockstates/stone.json` or `models/block/cube_all.json`.
#[derive(Default)]
struct ModelFiles {
    blockstates: Vec<(String, String)>,
    models: Vec<(String, String)>,
}

impl ModelPack {
    /// Loads every blockstate and model under `root/blockstates` and
    /// `root/models`, where `root` is the pack's `assets/minecraft` folder.
    ///
    /// Malformed files are an error; models that cannot be resolved (missing
    /// parent, parent cycle) are simply never selectable, matching the
    /// "unsupported -> `None`" contract of [`ModelSource`].
    pub fn load(root: &Path) -> Result<Self, String> {
        Self::from_model_files(read_directory(root)?, false)
    }

    /// Like [`Self::load`], but a malformed or unsupported blockstate/model
    /// file is skipped instead of failing the whole pack. Vanilla keeps the
    /// rest of a pack usable and substitutes its missing model; the lenient
    /// path mirrors that for real 1.16.5 data.
    pub fn load_lenient(root: &Path) -> Result<Self, String> {
        Self::from_model_files(read_directory(root)?, true)
    }

    /// Loads the vanilla blockstates and models straight from a 1.16.5
    /// client jar's `assets/minecraft` tree. Vanilla's own files are read
    /// leniently: a file this loader cannot bake is skipped, as vanilla
    /// substitutes its missing model, instead of rejecting the whole jar.
    pub fn load_client_jar(jar_path: &Path) -> Result<Self, String> {
        Self::from_model_files(read_client_jar(jar_path)?, true)
    }

    fn from_model_files(files: ModelFiles, lenient: bool) -> Result<Self, String> {
        let mut blockstates = HashMap::new();
        for (relative, text) in files.blockstates {
            let path = Path::new(&relative);
            let name = format!(
                "minecraft:{}",
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .ok_or_else(|| format!("{relative}: not a valid file name"))?
            );
            match parse_blockstate(path, &text) {
                Ok(definition) => {
                    blockstates.insert(name, definition);
                }
                Err(_) if lenient => {}
                Err(error) => return Err(error),
            }
        }

        let mut raw_models = HashMap::new();
        for (relative, text) in files.models {
            let mut name = relative
                .strip_prefix("models/")
                .unwrap_or(&relative)
                .to_owned();
            if name.ends_with(".json") {
                name.truncate(name.len() - ".json".len());
            }
            let name = format!("minecraft:{name}");
            match parse_model(Path::new(&relative), &text) {
                Ok(model) => {
                    raw_models.insert(name, model);
                }
                Err(_) if lenient => {}
                Err(error) => return Err(error),
            }
        }
        let models = raw_models
            .keys()
            .map(|name| (name.clone(), resolve_model(name, &raw_models)))
            .collect();
        Ok(Self {
            blockstates,
            models,
            cache: RefCell::new(HashMap::new()),
        })
    }

    /// Bakes the model selected for `state`, or `None` when the block or its
    /// variant cannot be selected.
    ///
    /// Results are memoized per state, so repeated calls return equal clones
    /// of the first bake. Selection itself still runs against the immutable
    /// blockstate and model tables, so the cache is invisible to callers.
    pub fn model(&self, state: &BlockState) -> Option<BakedModel> {
        let cached = self.cache.borrow().get(state).cloned();
        if let Some(cached) = cached {
            return cached;
        }
        let baked = self.bake(state);
        self.cache.borrow_mut().insert(state.clone(), baked.clone());
        baked
    }

    fn bake(&self, state: &BlockState) -> Option<BakedModel> {
        let name = namespaced(&state.name);
        let definition = self.blockstates.get(&name)?;
        if !definition.variants.is_empty() {
            let entry = definition
                .variants
                .iter()
                .find(|entry| variant_key_matches(&entry.key, state))?;
            return self.bake_apply(&entry.apply);
        }
        let mut quads = Vec::new();
        for part in &definition.multipart {
            if part
                .when
                .as_ref()
                .is_none_or(|condition| condition.matches(state))
            {
                if let Some(mut baked) = self.bake_apply(&part.apply) {
                    quads.append(&mut baked.quads);
                }
            }
        }
        Some(BakedModel { quads })
    }

    fn bake_apply(&self, apply: &Apply) -> Option<BakedModel> {
        let variant = apply.entries.first()?;
        let model = self.models.get(&variant.model)?.as_ref()?;
        let transform = BlockTransform::new(variant.x, variant.y, variant.uvlock);
        let mut quads = Vec::new();
        for element in &model.elements {
            bake_element(element, &transform, &mut quads);
        }
        Some(BakedModel { quads })
    }

    /// The inventory-render model of `item`, with the texture names it uses.
    ///
    /// A name with a blockstate file is a block item and bakes its `block/`
    /// model; everything else falls back to its `item/` model, matching how
    /// 1.16.5 names block and item models. Item models such as
    /// `item/generated` carry no elements, so `model` may be empty while
    /// `textures` still lists the sprite layers a caller should draw.
    ///
    /// Nothing is rasterized here: the quads are in model space (0..16) and
    /// the texture names are resolved sprite names such as
    /// `minecraft:block/stone`. Returns `None` when the model is unknown or
    /// could not be resolved.
    pub fn render_item(&self, item: &str) -> Option<ItemRender> {
        let name = namespaced(item);
        let path = name.strip_prefix("minecraft:")?;
        let model_name = if self.blockstates.contains_key(&name) {
            format!("minecraft:block/{path}")
        } else {
            format!("minecraft:item/{path}")
        };
        let resolved = self.models.get(&model_name)?.as_ref()?;
        let mut quads = Vec::new();
        let transform = BlockTransform::new(0, 0, false);
        for element in &resolved.elements {
            bake_element(element, &transform, &mut quads);
        }
        let mut textures: Vec<String> = if quads.is_empty() {
            resolved.textures.values().cloned().collect()
        } else {
            quads.iter().map(|quad| quad.texture.clone()).collect()
        };
        textures.sort();
        textures.dedup();
        Some(ItemRender {
            model: BakedModel { quads },
            textures,
        })
    }
}

impl ModelSource for ModelPack {
    fn model(&self, state: &BlockState) -> Option<BakedModel> {
        ModelPack::model(self, state)
    }
}

/// A blockstate x/y quarter-turn rotation with optional face UV locking.
#[derive(Clone, Copy)]
struct BlockTransform {
    matrix: Mat3,
    uvlock: bool,
}

impl BlockTransform {
    fn new(x: u8, y: u8, uvlock: bool) -> Self {
        Self {
            matrix: rotation_matrix(x, y),
            uvlock,
        }
    }

    /// Rotates a 0..16 model-space point around the block centre (8,8,8).
    fn position(self, point: [f32; 3]) -> [f32; 3] {
        let relative = [point[0] - 8.0, point[1] - 8.0, point[2] - 8.0];
        let rotated = self.matrix.position(relative);
        [rotated[0] + 8.0, rotated[1] + 8.0, rotated[2] + 8.0]
    }

    /// Rotates a 0..16 face UV around the face centre (8,8) when UV-locked;
    /// unlocked UVs move with the face and are returned untouched.
    fn uv(self, direction: Face, uv: [f32; 2]) -> [f32; 2] {
        if !self.uvlock {
            return uv;
        }
        let matrix = uvlock_matrix(&self.matrix, direction);
        let rotated = matrix.position([uv[0] - 8.0, uv[1] - 8.0, 0.0]);
        [rotated[0] + 8.0, rotated[1] + 8.0]
    }

    /// Rotates a cullface direction with the block.
    fn direction(self, direction: Face) -> Face {
        nearest_face(self.matrix.position(direction.normal()))
    }
}

/// A 3x3 matrix in row convention: `position(v) = rows . v`.
#[derive(Clone, Copy, Debug)]
struct Mat3 {
    rows: [[f32; 3]; 3],
}

impl Mat3 {
    const IDENTITY: Self = Self {
        rows: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    };

    fn mul(self, other: Self) -> Self {
        let mut rows = [[0.0; 3]; 3];
        for row in 0..3 {
            for column in 0..3 {
                rows[row][column] = self.rows[row][0] * other.rows[0][column]
                    + self.rows[row][1] * other.rows[1][column]
                    + self.rows[row][2] * other.rows[2][column];
            }
        }
        Self { rows }
    }

    /// Inverts a rotation matrix (every matrix built here is one).
    fn inverse(self) -> Self {
        let mut rows = [[0.0; 3]; 3];
        for row in 0..3 {
            for column in 0..3 {
                rows[row][column] = self.rows[column][row];
            }
        }
        Self { rows }
    }

    fn position(self, vector: [f32; 3]) -> [f32; 3] {
        [
            self.rows[0][0] * vector[0] + self.rows[0][1] * vector[1] + self.rows[0][2] * vector[2],
            self.rows[1][0] * vector[0] + self.rows[1][1] * vector[1] + self.rows[1][2] * vector[2],
            self.rows[2][0] * vector[0] + self.rows[2][1] * vector[1] + self.rows[2][2] * vector[2],
        ]
    }
}

/// The x-then-y quarter-turn block rotation, built the way vanilla composes
/// `ModelRotation`: negative quarter turns around X, then around Y.
fn rotation_matrix(x: u8, y: u8) -> Mat3 {
    let mut rows = Mat3::IDENTITY.rows;
    for (axis, turns) in [(0usize, x), (1, y)] {
        for _ in 0..turns {
            let a = (axis + 1) % 3;
            let b = (axis + 2) % 3;
            let old = rows[a];
            rows[a] = rows[b];
            rows[b] = old.map(|value| -value);
        }
    }
    Mat3 { rows }
}

/// The rotation that carries the south face's UV plane onto each face's own,
/// as `FaceBakery` needs it to counter-rotate locked UVs.
fn face_basis(direction: Face) -> Mat3 {
    let rows = match direction {
        Face::South => Mat3::IDENTITY.rows,
        Face::Down => [[1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]],
        Face::Up => [[1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0]],
        Face::East => [[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0]],
        Face::West => [[0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]],
        Face::North => [[-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0]],
    };
    Mat3 { rows }
}

/// The UV-lock matrix for one face direction: the face-local inverse of the
/// block rotation, so locked UVs keep their world orientation.
fn uvlock_matrix(matrix: &Mat3, direction: Face) -> Mat3 {
    let basis = face_basis(direction);
    let transformed = matrix.mul(basis);
    let normal = [
        transformed.rows[0][2],
        transformed.rows[1][2],
        transformed.rows[2][2],
    ];
    let rotated_face = nearest_face(normal);
    face_basis(rotated_face)
        .inverse()
        .mul(transformed)
        .inverse()
}

/// The element rotation matrix, with optional rescale: each column divided by
/// the largest absolute component so the rotated box stays inside its slot.
fn element_matrix(rotation: &ElementRotation) -> Mat3 {
    let angle = rotation.angle * (std::f32::consts::PI / 180.0);
    let (sin, cos) = angle.sin_cos();
    let mut rows = match rotation.axis {
        Axis::X => [[1.0, 0.0, 0.0], [0.0, cos, -sin], [0.0, sin, cos]],
        Axis::Y => [[cos, 0.0, sin], [0.0, 1.0, 0.0], [-sin, 0.0, cos]],
        Axis::Z => [[cos, -sin, 0.0], [sin, cos, 0.0], [0.0, 0.0, 1.0]],
    };
    if rotation.rescale {
        for column in 0..3 {
            let maximum = rows[0][column]
                .abs()
                .max(rows[1][column].abs())
                .max(rows[2][column].abs());
            if maximum > 0.0 {
                let scale = 1.0 / maximum;
                for row in &mut rows {
                    row[column] *= scale;
                }
            }
        }
    }
    Mat3 { rows }
}

/// The four corners of one box face in 0..16 space, counter-clockwise from
/// outside, in `FaceBakery`'s order.
fn corners(from: [f32; 3], to: [f32; 3], direction: Face) -> [[f32; 3]; 4] {
    let selectors = match direction {
        Face::Down => [[0, 0, 1], [0, 0, 0], [1, 0, 0], [1, 0, 1]],
        Face::Up => [[0, 1, 0], [0, 1, 1], [1, 1, 1], [1, 1, 0]],
        Face::North => [[1, 1, 0], [1, 0, 0], [0, 0, 0], [0, 1, 0]],
        Face::South => [[0, 1, 1], [0, 0, 1], [1, 0, 1], [1, 1, 1]],
        Face::West => [[0, 1, 0], [0, 0, 0], [0, 0, 1], [0, 1, 1]],
        Face::East => [[1, 1, 1], [1, 0, 1], [1, 0, 0], [1, 1, 0]],
    };
    selectors.map(|selector| {
        std::array::from_fn(|axis| {
            if selector[axis] == 0 {
                from[axis]
            } else {
                to[axis]
            }
        })
    })
}

/// The face's UV rectangle when the model leaves it implicit, as vanilla's
/// `FaceBakery` fills it from the element bounds.
fn default_uv(from: [f32; 3], to: [f32; 3], direction: Face) -> [f32; 4] {
    match direction {
        Face::Down => [from[0], 16.0 - to[2], to[0], 16.0 - from[2]],
        Face::Up => [from[0], from[2], to[0], to[2]],
        Face::North => [16.0 - to[0], 16.0 - to[1], 16.0 - from[0], 16.0 - from[1]],
        Face::South => [from[0], 16.0 - to[1], to[0], 16.0 - from[1]],
        Face::West => [from[2], 16.0 - to[1], to[2], 16.0 - from[1]],
        Face::East => [16.0 - to[2], 16.0 - to[1], 16.0 - from[2], 16.0 - from[1]],
    }
}

/// The direction a quad faces, from its first three vertices.
fn facing(vertices: &[[f32; 3]; 4]) -> Option<Face> {
    let a = [
        vertices[1][0] - vertices[0][0],
        vertices[1][1] - vertices[0][1],
        vertices[1][2] - vertices[0][2],
    ];
    let b = [
        vertices[2][0] - vertices[0][0],
        vertices[2][1] - vertices[0][1],
        vertices[2][2] - vertices[0][2],
    ];
    let normal = [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ];
    let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
    if !length.is_finite() || length == 0.0 {
        return None;
    }
    let normal = normal.map(|value| value / length);
    Some(nearest_face(normal))
}

/// The direction whose normal best matches `normal` (exact for the axis
/// aligned vectors produced by quarter-turn matrices).
fn nearest_face(normal: [f32; 3]) -> Face {
    let dots = [
        -normal[1], normal[1], -normal[2], normal[2], -normal[0], normal[0],
    ];
    let mut best = f32::MIN_POSITIVE;
    let mut face = Face::North;
    for (direction, dot) in Face::ALL.iter().zip(dots) {
        if dot > best {
            best = dot;
            face = *direction;
        }
    }
    face
}

/// Bakes one element into quads, in `Face::ALL` order per element.
fn bake_element(element: &Element, transform: &BlockTransform, out: &mut Vec<Quad>) {
    let flat = |axis: usize| element.from[axis] == element.to[axis];
    for (index, face) in element.faces.iter().enumerate() {
        let Some(face) = face else { continue };
        let direction = Face::ALL[index];
        // As vanilla block baking does: a face is only drawn when the element
        // has extent along both of the face's axes.
        let drawn = match direction {
            Face::West | Face::East => !flat(1) && !flat(2),
            Face::Down | Face::Up => !flat(0) && !flat(2),
            Face::North | Face::South => !flat(0) && !flat(1),
        };
        if drawn {
            out.push(bake_face(element, face, direction, transform));
        }
    }
}

/// Bakes one face into a quad, mirroring `FaceBakery.bakeQuad`: element
/// rotation first, then the block transform, then the UV assignment (face
/// rotation and UV lock), and finally the canonical vertex order for the
/// quad's new facing so the vertices wind counter-clockwise from outside.
fn bake_face(
    element: &Element,
    face: &RawFace,
    direction: Face,
    transform: &BlockTransform,
) -> Quad {
    let mut positions = corners(element.from, element.to, direction);
    if let Some(rotation) = &element.rotation {
        let matrix = element_matrix(rotation);
        for vertex in &mut positions {
            let relative = [
                vertex[0] - rotation.origin[0],
                vertex[1] - rotation.origin[1],
                vertex[2] - rotation.origin[2],
            ];
            let rotated = matrix.position(relative);
            *vertex = [
                rotated[0] + rotation.origin[0],
                rotated[1] + rotation.origin[1],
                rotated[2] + rotation.origin[2],
            ];
        }
    }
    positions = positions.map(|vertex| transform.position(vertex));
    let bounds = face
        .uv
        .unwrap_or_else(|| default_uv(element.from, element.to, direction));
    let mut uv: [[f32; 2]; 4] = std::array::from_fn(|index| {
        let corner = (index + usize::from(face.rotation)) % 4;
        transform.uv(
            direction,
            [
                bounds[if corner < 2 { 0 } else { 2 }],
                bounds[if corner == 0 || corner == 3 { 1 } else { 3 }],
            ],
        )
    });
    let normal = facing(&positions);
    // Vanilla only re-canonicalizes the vertex order for faces without an
    // element rotation; rotated elements keep `setupShape`'s order.
    if element.rotation.is_none() {
        if let Some(quad_facing) = normal {
            let mut min = [f32::MAX; 3];
            let mut max = [f32::MIN; 3];
            for vertex in &positions {
                for axis in 0..3 {
                    min[axis] = min[axis].min(vertex[axis]);
                    max[axis] = max[axis].max(vertex[axis]);
                }
            }
            for (index, expected) in corners(min, max, quad_facing).into_iter().enumerate() {
                let Some(found) = (index..4).find(|candidate| positions[*candidate] == expected)
                else {
                    break;
                };
                positions.swap(index, found);
                uv.swap(index, found);
            }
        }
    }
    let normal = normal.unwrap_or(Face::Up).normal();
    let cullface = face
        .cullface
        .map(|direction| transform.direction(direction));
    let vertices = std::array::from_fn(|index| Vertex {
        position: positions[index],
        uv: uv[index],
        normal,
    });
    Quad {
        vertices,
        texture: face.texture.clone(),
        cullface,
    }
}

/// Resolves one model: follows the parent chain (cycles and missing parents
/// make the model unselectable), merges the texture maps child-over-parent,
/// and resolves every face texture variable against the merged map.
fn resolve_model(name: &str, raw_models: &HashMap<String, RawModel>) -> Option<ResolvedModel> {
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut current = name.to_owned();
    loop {
        if chain.len() > 64 || !seen.insert(current.clone()) {
            return None;
        }
        let model = raw_models.get(&current)?;
        chain.push(model);
        match &model.parent {
            Some(parent) => current = parent.clone(),
            None => break,
        }
    }
    let mut textures = HashMap::new();
    for model in chain.iter().rev() {
        textures.extend(
            model
                .textures
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
    let mut elements = Vec::new();
    for model in &chain {
        if model.has_elements {
            elements = model
                .elements
                .iter()
                .map(|element| resolve_element(element, &textures))
                .collect();
            break;
        }
    }
    let resolved_textures: HashMap<String, String> = textures
        .iter()
        .map(|(key, value)| (key.clone(), resolve_texture(value, &textures)))
        .collect();
    Some(ResolvedModel {
        elements,
        textures: resolved_textures,
    })
}

/// Resolves an element's face textures against the merged texture map.
fn resolve_element(element: &Element, textures: &HashMap<String, String>) -> Element {
    let mut resolved = element.clone();
    for face in resolved.faces.iter_mut().flatten() {
        face.texture = resolve_texture(&face.texture, textures);
    }
    resolved
}

/// Resolves one texture reference: `#variable` chains through the texture map
/// to an absolute path, or falls back to the missing sprite.
fn resolve_texture(reference: &str, textures: &HashMap<String, String>) -> String {
    let Some(variable) = reference.strip_prefix('#') else {
        return namespaced(reference);
    };
    let mut name = variable.to_owned();
    for _ in 0..64 {
        let Some(value) = textures.get(&name) else {
            return MISSING_TEXTURE.to_owned();
        };
        match value.strip_prefix('#') {
            Some(next) => name = next.to_owned(),
            None => return namespaced(value),
        }
    }
    MISSING_TEXTURE.to_owned()
}

/// Does a variant key select this state? An empty key is the default and
/// matches everything; otherwise every `property=value` term must match.
fn variant_key_matches(key: &str, state: &BlockState) -> bool {
    if key.is_empty() {
        return true;
    }
    key.split(',').all(|term| {
        term.split_once('=').is_some_and(|(property, value)| {
            state
                .properties
                .get(property)
                .is_some_and(|found| found == value)
        })
    })
}

/// Namespaces a bare identifier under `minecraft`, leaving explicit
/// namespaces untouched.
fn namespaced(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}

/// Parses a blockstate file, recovering the variant map's insertion order
/// from the source text (serde_json maps are BTreeMap-backed and lose the
/// file order that vanilla variant selection depends on).
fn parse_blockstate(path: &Path, text: &str) -> Result<BlockstateDef, String> {
    let value: Value = serde_json::from_str(text).map_err(|message| file_error(path, message))?;
    let object = as_object(&value, "blockstate")?;
    let has_variants = object.contains_key("variants");
    let has_multipart = object.contains_key("multipart");
    if has_variants == has_multipart {
        return Err(file_error(
            path,
            if has_variants {
                "blockstate declares both \"variants\" and \"multipart\"".to_owned()
            } else {
                "blockstate declares neither \"variants\" nor \"multipart\"".to_owned()
            },
        ));
    }
    let entries = ordered_object(text).map_err(|message| file_error(path, message))?;
    let mut definition = BlockstateDef {
        variants: Vec::new(),
        multipart: Vec::new(),
    };
    if has_variants {
        let span = &entries
            .iter()
            .find(|(key, _)| key == "variants")
            .ok_or_else(|| file_error(path, "blockstate has no \"variants\" member"))?
            .1;
        let variants_text = &text[span.clone()];
        let variant_entries =
            ordered_object(variants_text).map_err(|message| file_error(path, message))?;
        for (key, value_span) in variant_entries {
            if !key.is_empty() {
                for term in key.split(',') {
                    match term.split_once('=') {
                        Some((property, value)) if !property.is_empty() && !value.is_empty() => {}
                        _ => {
                            return Err(file_error(
                                path,
                                format!("variant key {key:?} is not \"property=value\" terms"),
                            ));
                        }
                    }
                }
            }
            let value: Value = serde_json::from_str(&variants_text[value_span])
                .map_err(|message| file_error(path, message))?;
            definition.variants.push(VariantEntry {
                key,
                apply: parse_apply(&value).map_err(|message| file_error(path, message))?,
            });
        }
    } else {
        let span = &entries
            .iter()
            .find(|(key, _)| key == "multipart")
            .ok_or_else(|| file_error(path, "blockstate has no \"multipart\" member"))?
            .1;
        let value: Value = serde_json::from_str(&text[span.clone()])
            .map_err(|message| file_error(path, message))?;
        for part in
            as_array(&value, "\"multipart\"").map_err(|message| file_error(path, message))?
        {
            let part =
                as_object(part, "multipart part").map_err(|message| file_error(path, message))?;
            let when = match part.get("when") {
                None => None,
                Some(condition) => Some(
                    parse_condition(condition, "\"when\"")
                        .map_err(|message| file_error(path, message))?,
                ),
            };
            let apply = parse_apply(
                part.get("apply")
                    .ok_or_else(|| file_error(path, "multipart part has no \"apply\" member"))?,
            )
            .map_err(|message| file_error(path, message))?;
            definition.multipart.push(MultipartPart { when, apply });
        }
    }
    Ok(definition)
}

/// Parses a variant application: a bare model string, an object, or a
/// weighted list of objects.
fn parse_apply(value: &Value) -> Result<Apply, String> {
    let mut entries = Vec::new();
    match value {
        Value::String(model) => entries.push(Variant {
            model: namespaced(model),
            x: 0,
            y: 0,
            uvlock: false,
        }),
        Value::Object(_) => entries.push(parse_variant(value)?),
        Value::Array(items) => {
            if items.is_empty() {
                return Err("variant list is empty".into());
            }
            for item in items {
                entries.push(parse_variant(item)?);
            }
        }
        _ => return Err("variant must be a string, an object or an array".into()),
    }
    Ok(Apply { entries })
}

/// Parses one variant object: `model` plus optional `x`, `y`, `uvlock` and
/// `weight` (validated, then ignored; selection picks the first entry).
fn parse_variant(value: &Value) -> Result<Variant, String> {
    let object = as_object(value, "variant")?;
    let model = as_str(
        object
            .get("model")
            .ok_or_else(|| "variant has no \"model\" member".to_owned())?,
        "\"model\"",
    )?;
    let x = quarter(object.get("x").unwrap_or(&Value::from(0)), "\"x\"")?;
    let y = quarter(object.get("y").unwrap_or(&Value::from(0)), "\"y\"")?;
    let uvlock = match object.get("uvlock") {
        None => false,
        Some(value) => as_bool(value, "\"uvlock\"")?,
    };
    if let Some(weight) = object.get("weight") {
        let _ = as_int(weight, "\"weight\"")?;
    }
    Ok(Variant {
        model: namespaced(model),
        x,
        y,
        uvlock,
    })
}

/// Parses a multipart condition: `OR`, `AND`, or a set of property terms
/// (multiple keys are an AND), each `property: "!value1|value2"`.
fn parse_condition(value: &Value, what: &str) -> Result<Condition, String> {
    let object = as_object(value, what)?;
    if let Some(or) = object.get("OR") {
        let conditions = as_array(or, &format!("{what} \"OR\""))?
            .iter()
            .map(|condition| parse_condition(condition, what))
            .collect::<Result<Vec<_>, _>>()?;
        if conditions.is_empty() {
            return Err(format!("{what} \"OR\" has no entries"));
        }
        return Ok(Condition::Or(conditions));
    }
    if let Some(and) = object.get("AND") {
        let conditions = as_array(and, &format!("{what} \"AND\""))?
            .iter()
            .map(|condition| parse_condition(condition, what))
            .collect::<Result<Vec<_>, _>>()?;
        if conditions.is_empty() {
            return Err(format!("{what} \"AND\" has no entries"));
        }
        return Ok(Condition::And(conditions));
    }
    if object.is_empty() {
        return Err(format!("{what} has no properties"));
    }
    let mut terms = Vec::new();
    for (key, value) in object {
        let value = as_str(value, &format!("{what} entry {key:?}"))?;
        let (negated, rest) = match value.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, value),
        };
        terms.push(Condition::Key {
            key: key.clone(),
            negated,
            values: rest.split('|').map(str::to_owned).collect(),
        });
    }
    if terms.len() == 1 {
        Ok(terms.pop().expect("one term"))
    } else {
        Ok(Condition::And(terms))
    }
}

/// Parses a model file into its raw (unresolved) form.
fn parse_model(path: &Path, text: &str) -> Result<RawModel, String> {
    let value: Value = serde_json::from_str(text).map_err(|message| file_error(path, message))?;
    let object = as_object(&value, "model")?;
    let parent = match object.get("parent") {
        None => None,
        Some(parent) => Some(namespaced(
            as_str(parent, "\"parent\"").map_err(|message| file_error(path, message))?,
        )),
    };
    let mut textures = HashMap::new();
    if let Some(entries) = object.get("textures") {
        if !entries.is_null() {
            for (key, value) in
                as_object(entries, "\"textures\"").map_err(|message| file_error(path, message))?
            {
                let value = as_str(value, &format!("\"textures\" entry {key:?}"))
                    .map_err(|message| file_error(path, message))?;
                textures.insert(key.clone(), value.to_owned());
            }
        }
    }
    let mut elements = Vec::new();
    let mut has_elements = false;
    match object.get("elements") {
        None | Some(Value::Null) => {}
        Some(list) => {
            has_elements = true;
            for element in
                as_array(list, "\"elements\"").map_err(|message| file_error(path, message))?
            {
                elements.push(parse_element(element).map_err(|message| file_error(path, message))?);
            }
        }
    }
    Ok(RawModel {
        parent,
        textures,
        elements,
        has_elements,
    })
}

/// Parses one element: `from`, `to`, optional `rotation` and `faces`.
fn parse_element(value: &Value) -> Result<Element, String> {
    let object = as_object(value, "element")?;
    let from = as_vec3(
        object
            .get("from")
            .ok_or_else(|| "element has no \"from\" member".to_owned())?,
        "\"from\"",
    )?;
    let to = as_vec3(
        object
            .get("to")
            .ok_or_else(|| "element has no \"to\" member".to_owned())?,
        "\"to\"",
    )?;
    let rotation = match object.get("rotation") {
        None | Some(Value::Null) => None,
        Some(rotation) => {
            let rotation = as_object(rotation, "\"rotation\"")?;
            let origin = as_vec3(
                rotation
                    .get("origin")
                    .ok_or_else(|| "\"rotation\" has no \"origin\" member".to_owned())?,
                "\"origin\"",
            )?;
            let axis = match as_str(
                rotation
                    .get("axis")
                    .ok_or_else(|| "\"rotation\" has no \"axis\" member".to_owned())?,
                "\"axis\"",
            )? {
                "x" => Axis::X,
                "y" => Axis::Y,
                "z" => Axis::Z,
                other => return Err(format!("\"axis\": unknown axis {other:?}")),
            };
            let angle = as_f64(
                rotation
                    .get("angle")
                    .ok_or_else(|| "\"rotation\" has no \"angle\" member".to_owned())?,
                "\"angle\"",
            )? as f32;
            let remainder = angle.rem_euclid(22.5);
            if remainder > 0.001 && 22.5 - remainder > 0.001 {
                return Err(format!(
                    "\"angle\": {angle} is not a multiple of 22.5 degrees"
                ));
            }
            let rescale = match rotation.get("rescale") {
                None => false,
                Some(rescale) => as_bool(rescale, "\"rescale\"")?,
            };
            Some(ElementRotation {
                origin,
                axis,
                angle,
                rescale,
            })
        }
    };
    let mut faces: [Option<RawFace>; 6] = Default::default();
    if let Some(entries) = object.get("faces") {
        if !entries.is_null() {
            for (key, face) in as_object(entries, "\"faces\"")? {
                let direction = parse_face_name(key, "\"faces\" key")?;
                faces[direction as usize] = Some(parse_face(face)?);
            }
        }
    }
    Ok(Element {
        from,
        to,
        rotation,
        faces,
    })
}

/// Parses one face: `texture` is required; `uv`, `cullface` and `rotation`
/// have defaults.
fn parse_face(value: &Value) -> Result<RawFace, String> {
    let object = as_object(value, "face")?;
    let texture = as_str(
        object
            .get("texture")
            .ok_or_else(|| "face has no \"texture\" member".to_owned())?,
        "\"texture\"",
    )?
    .to_owned();
    let uv = match object.get("uv") {
        None => None,
        Some(uv) => Some(as_vec4(uv, "\"uv\"")?),
    };
    let cullface = match object.get("cullface") {
        None => None,
        Some(cullface) => Some(parse_face_name(
            as_str(cullface, "\"cullface\"")?,
            "\"cullface\"",
        )?),
    };
    let rotation = match object.get("rotation") {
        None => 0,
        Some(rotation) => quarter(rotation, "\"rotation\"")?,
    };
    Ok(RawFace {
        uv,
        texture,
        cullface,
        rotation,
    })
}

/// Maps the six face names used by 1.16.5 block models.
fn parse_face_name(name: &str, what: &str) -> Result<Face, String> {
    match name {
        "down" => Ok(Face::Down),
        "up" => Ok(Face::Up),
        "north" => Ok(Face::North),
        "south" => Ok(Face::South),
        "west" => Ok(Face::West),
        "east" => Ok(Face::East),
        other => Err(format!("{what}: unknown face {other:?}")),
    }
}

/// A rotation in quarter turns (0, 90, 180 or 270 degrees).
fn quarter(value: &Value, what: &str) -> Result<u8, String> {
    match as_int(value, what)? {
        0 => Ok(0),
        90 => Ok(1),
        180 => Ok(2),
        270 => Ok(3),
        other => Err(format!("{what}: {other} is not a multiple of 90 degrees")),
    }
}

// --- JSON accessors ------------------------------------------------------

fn as_object<'a>(
    value: &'a Value,
    what: &str,
) -> Result<&'a serde_json::Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{what} must be a JSON object"))
}

fn as_array<'a>(value: &'a Value, what: &str) -> Result<&'a Vec<Value>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("{what} must be a JSON array"))
}

fn as_str<'a>(value: &'a Value, what: &str) -> Result<&'a str, String> {
    value
        .as_str()
        .ok_or_else(|| format!("{what} must be a string"))
}

fn as_f64(value: &Value, what: &str) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|number| number.is_finite())
        .ok_or_else(|| format!("{what} must be a finite number"))
}

fn as_int(value: &Value, what: &str) -> Result<i64, String> {
    if let Some(integer) = value.as_i64() {
        return Ok(integer);
    }
    if let Some(number) = value.as_f64() {
        if number.is_finite() && number.fract() == 0.0 && number.abs() < 9.2e18 {
            return Ok(number as i64);
        }
    }
    Err(format!("{what} must be an integer"))
}

fn as_bool(value: &Value, what: &str) -> Result<bool, String> {
    value
        .as_bool()
        .ok_or_else(|| format!("{what} must be a boolean"))
}

fn as_vec3(value: &Value, what: &str) -> Result<[f32; 3], String> {
    let entries = as_array(value, what)?;
    if entries.len() != 3 {
        return Err(format!("{what} must have exactly 3 numbers"));
    }
    let mut out = [0.0; 3];
    for (index, entry) in entries.iter().enumerate() {
        out[index] = as_f64(entry, what)? as f32;
    }
    Ok(out)
}

fn as_vec4(value: &Value, what: &str) -> Result<[f32; 4], String> {
    let entries = as_array(value, what)?;
    if entries.len() != 4 {
        return Err(format!("{what} must have exactly 4 numbers"));
    }
    let mut out = [0.0; 4];
    for (index, entry) in entries.iter().enumerate() {
        out[index] = as_f64(entry, what)? as f32;
    }
    Ok(out)
}

// --- Ordered JSON object scanning -----------------------------------------

/// Returns the key -> raw value span pairs of one JSON object in file order.
/// serde_json builds BTreeMap-backed objects and loses the insertion order
/// that vanilla variant selection depends on, so the variant map is scanned
/// from the (already validated) source text instead.
fn ordered_object(text: &str) -> Result<Vec<(String, Range<usize>)>, String> {
    let mut scan = Scan {
        bytes: text.as_bytes(),
        pos: 0,
    };
    scan.skip_whitespace();
    if scan.take() != Some(b'{') {
        return Err("expected a JSON object".into());
    }
    let mut entries = Vec::new();
    scan.skip_whitespace();
    if scan.peek() == Some(b'}') {
        scan.take();
        return Ok(entries);
    }
    loop {
        scan.skip_whitespace();
        let key = scan.parse_string()?;
        scan.skip_whitespace();
        if scan.take() != Some(b':') {
            return Err(format!("expected ':' after key {key:?}"));
        }
        scan.skip_whitespace();
        let span = scan.value_span()?;
        entries.push((key, span));
        scan.skip_whitespace();
        match scan.take() {
            Some(b',') => {}
            Some(b'}') => return Ok(entries),
            _ => return Err("expected ',' or '}' in object".into()),
        }
    }
}

/// A byte scanner over one JSON document, used only to recover object member
/// order and spans. Well-formedness is already guaranteed by serde_json.
struct Scan<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Scan<'a> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn take(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        Some(byte)
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    /// Scans a JSON string (the cursor is at its opening quote) and returns
    /// its decoded contents, letting serde_json handle the escapes.
    fn parse_string(&mut self) -> Result<String, String> {
        if self.take() != Some(b'"') {
            return Err("expected a JSON string".into());
        }
        let opening = self.pos - 1;
        let closing = self.string_end()?;
        let literal = std::str::from_utf8(&self.bytes[opening..=closing])
            .map_err(|_| "string is not valid UTF-8".to_owned())?;
        serde_json::from_str(literal).map_err(|error| error.to_string())
    }

    /// Advances past a string's closing quote (the cursor is at the content).
    fn string_end(&mut self) -> Result<usize, String> {
        let mut pos = self.pos;
        loop {
            match *self
                .bytes
                .get(pos)
                .ok_or_else(|| "unterminated string".to_owned())?
            {
                b'"' => {
                    self.pos = pos + 1;
                    return Ok(pos);
                }
                b'\\' => pos += 2,
                _ => pos += 1,
            }
        }
    }

    /// Scans one balanced JSON value and returns its span.
    fn value_span(&mut self) -> Result<Range<usize>, String> {
        let start = self.pos;
        match self
            .peek()
            .ok_or_else(|| "unexpected end of input".to_owned())?
        {
            b'{' => self.balanced(b'{', b'}')?,
            b'[' => self.balanced(b'[', b']')?,
            b'"' => {
                self.parse_string()?;
            }
            b't' => self.keyword(b"true")?,
            b'f' => self.keyword(b"false")?,
            b'n' => self.keyword(b"null")?,
            byte if byte == b'-' || byte.is_ascii_digit() => {
                while matches!(
                    self.peek(),
                    Some(byte) if byte != b',' && byte != b'}' && byte != b']' && !byte.is_ascii_whitespace()
                ) {
                    self.pos += 1;
                }
            }
            other => return Err(format!("unexpected byte {other} in JSON value")),
        }
        Ok(start..self.pos)
    }

    /// Advances past one balanced `open`..`close` structure.
    fn balanced(&mut self, open: u8, close: u8) -> Result<(), String> {
        let mut depth = 0;
        while let Some(byte) = self.take() {
            match byte {
                b'"' => {
                    self.pos -= 1;
                    self.parse_string()?;
                }
                byte if byte == open => depth += 1,
                byte if byte == close => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                _ => {}
            }
        }
        Err("unterminated JSON value".into())
    }

    fn keyword(&mut self, word: &[u8]) -> Result<(), String> {
        if self
            .bytes
            .get(self.pos..)
            .is_some_and(|rest| rest.starts_with(word))
        {
            self.pos += word.len();
            Ok(())
        } else {
            Err(format!("invalid literal at offset {}", self.pos))
        }
    }
}

// --- Loading helpers --------------------------------------------------------

/// Reads the blockstate and model JSON of an extracted
/// `assets/minecraft` folder. Subdirectories are walked recursively; paths
/// in the result are relative to `root` and slash-separated.
fn read_directory(root: &Path) -> Result<ModelFiles, String> {
    let blockstates_dir = root.join("blockstates");
    let models_dir = root.join("models");
    if !blockstates_dir.is_dir() {
        return Err(format!("{}: not a directory", blockstates_dir.display()));
    }
    if !models_dir.is_dir() {
        return Err(format!("{}: not a directory", models_dir.display()));
    }
    let mut blockstate_paths = Vec::new();
    let mut model_paths = Vec::new();
    collect_json(&blockstates_dir, &mut blockstate_paths)?;
    collect_json(&models_dir, &mut model_paths)?;
    let mut files = ModelFiles::default();
    for path in blockstate_paths {
        let relative = path
            .strip_prefix(root)
            .map_err(|_| format!("{}: outside the pack", path.display()))?;
        files.blockstates.push((
            relative.to_string_lossy().replace('\\', "/"),
            read_json(&path)?,
        ));
    }
    for path in model_paths {
        let relative = path
            .strip_prefix(root)
            .map_err(|_| format!("{}: outside the pack", path.display()))?;
        files.models.push((
            relative.to_string_lossy().replace('\\', "/"),
            read_json(&path)?,
        ));
    }
    Ok(files)
}

/// Reads the blockstate and model JSON of a client jar's
/// `assets/minecraft` tree.
fn read_client_jar(jar_path: &Path) -> Result<ModelFiles, String> {
    use std::io::Read;
    let file = std::fs::File::open(jar_path).map_err(|e| format!("{}: {e}", jar_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("{}: not a client jar: {e}", jar_path.display()))?;
    let mut entries = Vec::new();
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|e| format!("{}: {e}", jar_path.display()))?;
        let Some(relative) = entry.name().strip_prefix("assets/minecraft/") else {
            continue;
        };
        if relative.ends_with(".json")
            && (relative.starts_with("blockstates/") || relative.starts_with("models/"))
        {
            entries.push(relative.to_owned());
        }
    }
    entries.sort();
    let mut files = ModelFiles::default();
    for relative in entries {
        let archive_name = format!("assets/minecraft/{relative}");
        let mut entry = archive
            .by_name(&archive_name)
            .map_err(|e| format!("{}: {e}", jar_path.display()))?;
        let mut text = String::new();
        entry
            .read_to_string(&mut text)
            .map_err(|e| format!("{archive_name}: {e}"))?;
        if relative.starts_with("models/") {
            files.models.push((relative, text));
        } else {
            files.blockstates.push((relative, text));
        }
    }
    Ok(files)
}

fn read_json(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
}

/// Prefixes a message with the path of the file it came from.
fn file_error(path: &Path, message: impl std::fmt::Display) -> String {
    format!("{}: {message}", path.display())
}

fn collect_json(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("{}: {error}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_json(&path, out)?;
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A vanilla-style cube whose faces use `#down`..`#east` variables.
    const CUBE: &str = r##"{
        "elements": [{
            "from": [0, 0, 0],
            "to": [16, 16, 16],
            "faces": {
                "down":  { "texture": "#down",  "cullface": "down" },
                "up":    { "texture": "#up",    "cullface": "up" },
                "north": { "texture": "#north", "cullface": "north" },
                "south": { "texture": "#south", "cullface": "south" },
                "west":  { "texture": "#west",  "cullface": "west" },
                "east":  { "texture": "#east",  "cullface": "east" }
            }
        }]
    }"##;

    static NEXT_PACK: AtomicUsize = AtomicUsize::new(0);

    fn pack_dir() -> PathBuf {
        let id = NEXT_PACK.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "horizon-1165-pack-{}-{id}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ))
    }

    fn write_pack(dir: &Path, files: &[(&str, &str)]) {
        for (relative, contents) in files {
            let path = dir.join(relative);
            std::fs::create_dir_all(path.parent().expect("files have a folder")).unwrap();
            std::fs::write(path, contents).unwrap();
        }
    }

    /// Loads a temp pack and removes it on drop.
    struct Pack {
        dir: PathBuf,
        pack: ModelPack,
    }

    impl Pack {
        fn new(files: &[(&str, &str)]) -> Self {
            let dir = pack_dir();
            write_pack(&dir, files);
            let pack = ModelPack::load(&dir).expect("pack loads");
            Self { dir, pack }
        }

        fn state(&self, name: &str, properties: &[(&str, &str)]) -> BlockState {
            BlockState {
                name: name.to_owned(),
                properties: properties
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                    .collect(),
            }
        }

        fn model(&self, name: &str, properties: &[(&str, &str)]) -> BakedModel {
            self.pack
                .model(&self.state(name, properties))
                .expect("model is selected")
        }

        fn textures(&self, name: &str, properties: &[(&str, &str)]) -> Vec<String> {
            self.model(name, properties)
                .quads
                .iter()
                .map(|quad| quad.texture.clone())
                .collect()
        }
    }

    impl Drop for Pack {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Loads a pack that is expected to fail and reports the error text.
    fn load_error(files: &[(&str, &str)]) -> String {
        let dir = pack_dir();
        write_pack(&dir, files);
        let result = ModelPack::load(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        result.expect_err("pack load fails")
    }

    const STONE_PACK: &[(&str, &str)] = &[
        ("models/block/cube.json", CUBE),
        (
            "models/block/cube_all.json",
            r##"{
                "parent": "block/cube",
                "textures": {
                    "particle": "#all", "down": "#all", "up": "#all",
                    "north": "#all", "south": "#all", "west": "#all", "east": "#all"
                }
            }"##,
        ),
        (
            "models/block/stone.json",
            r#"{"parent": "block/cube_all", "textures": {"all": "block/stone"}}"#,
        ),
        (
            "blockstates/stone.json",
            r#"{"variants": {"": {"model": "block/stone"}}}"#,
        ),
    ];

    #[test]
    fn cube_all_parent_chain_yields_six_quads() {
        let pack = Pack::new(STONE_PACK);
        let baked = pack.model("minecraft:stone", &[]);
        assert_eq!(baked.quads.len(), 6);
        for quad in &baked.quads {
            assert_eq!(quad.texture, "minecraft:block/stone");
            assert!(quad.cullface.is_some());
        }
        // Faces come out in the 1.16.5 direction order with matching cullfaces.
        let cullfaces: Vec<Face> = baked
            .quads
            .iter()
            .map(|quad| quad.cullface.unwrap())
            .collect();
        assert_eq!(
            cullfaces,
            vec![
                Face::Down,
                Face::Up,
                Face::North,
                Face::South,
                Face::West,
                Face::East
            ]
        );
        // The down face sits on y=0, the up face on y=16.
        for vertex in &baked.quads[0].vertices {
            assert_eq!(vertex.position[1], 0.0);
            assert_eq!(vertex.normal, [0.0, -1.0, 0.0]);
        }
        for vertex in &baked.quads[1].vertices {
            assert_eq!(vertex.position[1], 16.0);
        }
        // Default UVs and the north-face corner order.
        let north = &baked.quads[2];
        assert_eq!(north.texture, "minecraft:block/stone");
        let expected: Vec<([f32; 3], [f32; 2])> = vec![
            ([16.0, 16.0, 0.0], [0.0, 0.0]),
            ([16.0, 0.0, 0.0], [0.0, 16.0]),
            ([0.0, 0.0, 0.0], [16.0, 16.0]),
            ([0.0, 16.0, 0.0], [16.0, 0.0]),
        ];
        for (vertex, (position, uv)) in north.vertices.iter().zip(expected) {
            assert_eq!(vertex.position, position);
            assert_eq!(vertex.uv, uv);
        }
        // The trait object path resolves too.
        let source: &dyn ModelSource = &pack.pack;
        assert_eq!(
            source
                .model(&pack.state("minecraft:stone", &[]))
                .unwrap()
                .quads
                .len(),
            6
        );
    }

    #[test]
    fn texture_variables_follow_alias_chains_and_child_overrides() {
        let pack = Pack::new(&[
            (
                "models/block/pillar.json",
                r##"{"elements": [{
                    "from": [0, 0, 0], "to": [16, 16, 16],
                    "faces": {
                        "north": { "texture": "#side" },
                        "up":    { "texture": "#end" },
                        "down":  { "texture": "#end" }
                    }
                }]}"##,
            ),
            (
                "models/block/pillar_end.json",
                r##"{"parent": "block/pillar", "textures": {"side": "#side_of"}}"##,
            ),
            (
                "models/block/stone_pillar.json",
                r#"{
                    "parent": "block/pillar_end",
                    "textures": {"end": "block/end_stone", "side_of": "block/stone"}
                }"#,
            ),
            (
                "blockstates/stone_pillar.json",
                r#"{"variants": {"": {"model": "block/stone_pillar"}}}"#,
            ),
        ]);
        // Three quads in direction order: down and up (#end), north (#side).
        assert_eq!(
            pack.textures("minecraft:stone_pillar", &[]),
            vec![
                "minecraft:block/end_stone".to_owned(),
                "minecraft:block/end_stone".to_owned(),
                // #side -> #side_of -> block/stone
                "minecraft:block/stone".to_owned(),
            ]
        );
    }

    #[test]
    fn variants_match_properties_in_file_order_with_default() {
        // The "" default is written last; a sorted (BTreeMap) parse would
        // shadow the property keys. Vanilla picks the first match.
        let pack = Pack::new(&[
            (
                "models/block/axis_x.json",
                r#"{"elements": [{"from": [0,0,0], "to": [16,16,16],
                    "faces": {"up": {"texture": "block/x"}}}]}"#,
            ),
            (
                "models/block/axis_z.json",
                r#"{"elements": [{"from": [0,0,0], "to": [16,16,16],
                    "faces": {"up": {"texture": "block/z"}}}]}"#,
            ),
            (
                "models/block/axis_default.json",
                r#"{"elements": [{"from": [0,0,0], "to": [16,16,16],
                    "faces": {"up": {"texture": "block/default"}}}]}"#,
            ),
            (
                "blockstates/log.json",
                r#"{"variants": {
                    "axis=x": {"model": "block/axis_x"},
                    "axis=z": {"model": "block/axis_z"},
                    "": {"model": "block/axis_default"}
                }}"#,
            ),
        ]);
        assert_eq!(
            pack.textures("minecraft:log", &[("axis", "x")]),
            vec!["minecraft:block/x".to_owned()]
        );
        assert_eq!(
            pack.textures("minecraft:log", &[("axis", "z")]),
            vec!["minecraft:block/z".to_owned()]
        );
        // Unknown values and missing properties fall through to the default.
        assert_eq!(
            pack.textures("minecraft:log", &[("axis", "y")]),
            vec!["minecraft:block/default".to_owned()]
        );
        assert_eq!(
            pack.textures("minecraft:log", &[]),
            vec!["minecraft:block/default".to_owned()]
        );
        // A property key that names an absent property never matches.
        let pack = Pack::new(&[
            (
                "models/block/m.json",
                r#"{"elements": [{"from": [0,0,0], "to": [16,16,16],
                    "faces": {"up": {"texture": "block/m"}}}]}"#,
            ),
            (
                "blockstates/only.json",
                r#"{"variants": {"axis=x": {"model": "block/m"}}}"#,
            ),
        ]);
        assert!(
            pack.pack
                .model(&pack.state("minecraft:only", &[]))
                .is_none()
        );
    }

    #[test]
    fn y_rotation_turns_the_north_face_to_east() {
        let pack = Pack::new(&[
            (
                "models/block/face.json",
                r#"{"elements": [{"from": [0,0,0], "to": [16,16,16],
                    "faces": {"north": {"texture": "block/t", "uv": [0,0,16,16], "cullface": "north"}}}]}"#,
            ),
            (
                "blockstates/f.json",
                r#"{"variants": {
                    "facing=north": {"model": "block/face"},
                    "facing=east":  {"model": "block/face", "y": 90}
                }}"#,
            ),
        ]);
        // Unrotated: the north plane, canonical order, north cullface.
        let north = pack.model("minecraft:f", &[("facing", "north")]);
        assert_eq!(north.quads.len(), 1);
        let quad = &north.quads[0];
        assert_eq!(quad.cullface, Some(Face::North));
        for vertex in &quad.vertices {
            assert_eq!(vertex.position[2], 0.0);
            assert_eq!(vertex.normal, [0.0, 0.0, -1.0]);
        }
        // Rotated y=90: the quad moves to the east plane and the cullface
        // rotates with it.
        let east = pack.model("minecraft:f", &[("facing", "east")]);
        let quad = &east.quads[0];
        assert_eq!(quad.cullface, Some(Face::East));
        let expected: Vec<([f32; 3], [f32; 2])> = vec![
            ([16.0, 16.0, 16.0], [0.0, 0.0]),
            ([16.0, 0.0, 16.0], [0.0, 16.0]),
            ([16.0, 0.0, 0.0], [16.0, 16.0]),
            ([16.0, 16.0, 0.0], [16.0, 0.0]),
        ];
        for (vertex, (position, uv)) in quad.vertices.iter().zip(expected) {
            assert_eq!(vertex.position, position);
            assert_eq!(vertex.uv, uv);
            assert_eq!(vertex.normal, [1.0, 0.0, 0.0]);
        }
    }

    #[test]
    fn uvlock_keeps_the_top_texture_world_aligned() {
        let pack = Pack::new(&[
            (
                "models/block/top.json",
                r#"{"elements": [{"from": [0,0,0], "to": [16,16,16],
                    "faces": {"up": {"texture": "block/t", "uv": [0,0,8,16]}}}]}"#,
            ),
            (
                "blockstates/t.json",
                r#"{"variants": {
                    "rot=none":  {"model": "block/top"},
                    "rot=y":     {"model": "block/top", "y": 90},
                    "rot=ylock": {"model": "block/top", "y": 90, "uvlock": true}
                }}"#,
            ),
        ]);
        let positions = [
            [0.0, 16.0, 0.0],
            [0.0, 16.0, 16.0],
            [16.0, 16.0, 16.0],
            [16.0, 16.0, 0.0],
        ];
        let uvs = |name: &str| {
            let quad = &pack.model("minecraft:t", &[("rot", name)]).quads[0];
            let got: Vec<([f32; 3], [f32; 2])> = quad
                .vertices
                .iter()
                .map(|vertex| (vertex.position, vertex.uv))
                .collect();
            got
        };
        // The unrotated model: u runs along x, v along z.
        assert_eq!(
            uvs("none"),
            vec![
                (positions[0], [0.0, 0.0]),
                (positions[1], [0.0, 16.0]),
                (positions[2], [8.0, 16.0]),
                (positions[3], [8.0, 0.0]),
            ]
        );
        // Without uvlock the texture rotates with the block: u now runs
        // along z.
        assert_eq!(
            uvs("y"),
            vec![
                (positions[0], [0.0, 16.0]),
                (positions[1], [8.0, 16.0]),
                (positions[2], [8.0, 0.0]),
                (positions[3], [0.0, 0.0]),
            ]
        );
        // With uvlock the world orientation is restored: u along x again.
        assert_eq!(
            uvs("ylock"),
            vec![
                (positions[0], [0.0, 0.0]),
                (positions[1], [0.0, 8.0]),
                (positions[2], [16.0, 8.0]),
                (positions[3], [16.0, 0.0]),
            ]
        );
    }

    #[test]
    fn multipart_conditions_select_parts() {
        let pack = Pack::new(&[
            (
                "models/block/a.json",
                r#"{"elements": [{"from": [0,0,0], "to": [16,16,16],
                    "faces": {"up": {"texture": "block/a"}}}]}"#,
            ),
            (
                "models/block/b.json",
                r#"{"elements": [{"from": [0,0,0], "to": [16,16,16],
                    "faces": {"up": {"texture": "block/b"}}}]}"#,
            ),
            (
                "models/block/c.json",
                r#"{"elements": [{"from": [0,0,0], "to": [16,16,16],
                    "faces": {"up": {"texture": "block/c"}}}]}"#,
            ),
            (
                "models/block/d.json",
                r#"{"elements": [{"from": [0,0,0], "to": [16,16,16],
                    "faces": {"up": {"texture": "block/d"}}}]}"#,
            ),
            (
                "models/block/e.json",
                r#"{"elements": [{"from": [0,0,0], "to": [16,16,16],
                    "faces": {"up": {"texture": "block/e"}}}]}"#,
            ),
            (
                "models/block/f.json",
                r#"{"elements": [{"from": [0,0,0], "to": [16,16,16],
                    "faces": {"up": {"texture": "block/f"}}}]}"#,
            ),
            (
                "blockstates/multi.json",
                r#"{"multipart": [
                    {"when": {"north": "true"}, "apply": {"model": "block/a"}},
                    {"when": {"OR": [{"east": "true"}, {"west": "true"}]}, "apply": {"model": "block/b"}},
                    {"when": {"south": "false", "up": "false"}, "apply": {"model": "block/c"}},
                    {"when": {"down": "!false"}, "apply": {"model": "block/d"}},
                    {"when": {"north": "false|true"}, "apply": {"model": "block/f"}},
                    {"apply": {"model": "block/e"}}
                ]}"#,
            ),
        ]);
        let textures = |north: &str, east: &str, west: &str, south: &str, up: &str, down: &str| {
            pack.textures(
                "minecraft:multi",
                &[
                    ("north", north),
                    ("east", east),
                    ("west", west),
                    ("south", south),
                    ("up", up),
                    ("down", down),
                ],
            )
        };
        // north=true adds a; south&up false add c; down != false adds d;
        // the pipe matches any north; e always applies. Neither east nor west
        // is true, so b stays out.
        assert_eq!(
            textures("true", "false", "false", "false", "false", "true"),
            vec![
                "minecraft:block/a".to_owned(),
                "minecraft:block/c".to_owned(),
                "minecraft:block/d".to_owned(),
                "minecraft:block/f".to_owned(),
                "minecraft:block/e".to_owned(),
            ]
        );
        // east=true adds b; d's negation no longer matches; the rest stays.
        assert_eq!(
            textures("false", "true", "false", "true", "true", "false"),
            vec![
                "minecraft:block/b".to_owned(),
                "minecraft:block/f".to_owned(),
                "minecraft:block/e".to_owned(),
            ]
        );
    }

    #[test]
    fn element_rotation_spins_a_face_around_its_origin() {
        let pack = Pack::new(&[
            (
                "models/block/spun.json",
                r#"{"elements": [{
                    "from": [0,0,0], "to": [16,16,16],
                    "rotation": {"origin": [8,8,8], "axis": "y", "angle": 90, "rescale": false},
                    "faces": {"north": {"texture": "block/t", "uv": [0,0,16,16]}}
                }]}"#,
            ),
            (
                "blockstates/spun.json",
                r#"{"variants": {"": {"model": "block/spun"}}}"#,
            ),
        ]);
        let baked = pack.model("minecraft:spun", &[]);
        assert_eq!(baked.quads.len(), 1);
        let quad = &baked.quads[0];
        assert_eq!(quad.cullface, None);
        // The north face now lies on the west plane, in setup order.
        let expected: Vec<([f32; 3], [f32; 2])> = vec![
            ([0.0, 16.0, 0.0], [0.0, 0.0]),
            ([0.0, 0.0, 0.0], [0.0, 16.0]),
            ([0.0, 0.0, 16.0], [16.0, 16.0]),
            ([0.0, 16.0, 16.0], [16.0, 0.0]),
        ];
        for (vertex, (position, uv)) in quad.vertices.iter().zip(expected) {
            assert_close(vertex.position, position);
            assert_eq!(vertex.uv, uv);
            assert_eq!(vertex.normal, [-1.0, 0.0, 0.0]);
        }
    }

    /// Element rotations go through f32 trig, so positions carry the same
    /// rounding noise vanilla's FaceBakery produces; compare within 1e-4.
    fn assert_close(actual: [f32; 3], expected: [f32; 3]) {
        for (actual, expected) in actual.iter().zip(expected) {
            assert!(
                (actual - expected).abs() < 1e-4,
                "expected {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn malformed_packs_are_rejected() {
        const MODEL_X: &str = r#"{"elements": [{"from": [0,0,0], "to": [16,16,16],
            "faces": {"up": {"texture": "block/x"}}}]}"#;
        let model_x: &[(&str, &str)] = &[("models/block/x.json", MODEL_X)];
        for files in [
            // Broken JSON.
            vec![("blockstates/stone.json", "{")],
            // Both variants and multipart.
            vec![(
                "blockstates/stone.json",
                r#"{"variants": {"": "block/x"}, "multipart": [{"apply": "block/x"}]}"#,
            )],
            // Neither.
            vec![("blockstates/stone.json", r#"{"foo": 1}"#)],
            // Rotation not a quarter turn.
            vec![(
                "blockstates/stone.json",
                r#"{"variants": {"": {"model": "block/x", "x": 45}}}"#,
            )],
            // Malformed variant key.
            vec![(
                "blockstates/stone.json",
                r#"{"variants": {"axisx": {"model": "block/x"}}}"#,
            )],
            // Empty variant list.
            vec![("blockstates/stone.json", r#"{"variants": {"": []}}"#)],
            // Face rotation not a quarter turn.
            vec![(
                "models/block/x.json",
                r#"{"elements": [{"from": [0,0,0], "to": [16,16,16],
                    "faces": {"up": {"texture": "block/x", "rotation": 45}}}]}"#,
            )],
            // Unknown face.
            vec![(
                "models/block/x.json",
                r#"{"elements": [{"from": [0,0,0], "to": [16,16,16],
                    "faces": {"diagonal": {"texture": "block/x"}}}]}"#,
            )],
            // Face without a texture.
            vec![(
                "models/block/x.json",
                r#"{"elements": [{"from": [0,0,0], "to": [16,16,16],
                    "faces": {"up": {"uv": [0,0,16,16]}}}]}"#,
            )],
            // Unknown cullface.
            vec![(
                "models/block/x.json",
                r#"{"elements": [{"from": [0,0,0], "to": [16,16,16],
                    "faces": {"up": {"texture": "block/x", "cullface": "sideways"}}}]}"#,
            )],
            // Element rotation not a 22.5-degree step.
            vec![(
                "models/block/x.json",
                r#"{"elements": [{"from": [0,0,0], "to": [16,16,16],
                    "rotation": {"origin": [8,8,8], "axis": "y", "angle": 30},
                    "faces": {"up": {"texture": "block/x"}}}]}"#,
            )],
            // Unknown rotation axis.
            vec![(
                "models/block/x.json",
                r#"{"elements": [{"from": [0,0,0], "to": [16,16,16],
                    "rotation": {"origin": [8,8,8], "axis": "w", "angle": 45},
                    "faces": {"up": {"texture": "block/x"}}}]}"#,
            )],
        ] {
            let mut all = files;
            for entry in model_x {
                if !all.iter().any(|(path, _)| *path == entry.0) {
                    all.push(*entry);
                }
            }
            assert!(!load_error(&all).is_empty());
        }
    }

    #[test]
    fn unsupported_states_yield_none() {
        let pack = Pack::new(&[
            ("models/block/cube.json", CUBE),
            (
                "models/block/cube_all.json",
                r##"{"parent": "block/cube",
                    "textures": {"particle": "#all", "down": "#all", "up": "#all",
                                 "north": "#all", "south": "#all", "west": "#all", "east": "#all"}}"##,
            ),
            (
                "models/block/stone.json",
                r#"{"parent": "block/cube_all", "textures": {"all": "block/stone"}}"#,
            ),
            (
                "blockstates/stone.json",
                r#"{"variants": {"": {"model": "block/stone"}}}"#,
            ),
            // Property keys only: no default, so other values do not select.
            (
                "models/block/only.json",
                r#"{"elements": [{"from": [0,0,0], "to": [16,16,16],
                    "faces": {"up": {"texture": "block/only"}}}]}"#,
            ),
            (
                "blockstates/only.json",
                r#"{"variants": {"axis=x": {"model": "block/only"}}}"#,
            ),
            // References a model that is not in the pack.
            (
                "blockstates/ghost.json",
                r#"{"variants": {"": {"model": "block/ghost"}}}"#,
            ),
            // A parent chain that never resolves.
            ("models/block/broken.json", r#"{"parent": "block/nope"}"#),
            (
                "blockstates/broken.json",
                r#"{"variants": {"": {"model": "block/broken"}}}"#,
            ),
            // A parent cycle.
            (
                "models/block/cycle_a.json",
                r#"{"parent": "block/cycle_b"}"#,
            ),
            (
                "models/block/cycle_b.json",
                r#"{"parent": "block/cycle_a"}"#,
            ),
            (
                "blockstates/cycle.json",
                r#"{"variants": {"": {"model": "block/cycle_a"}}}"#,
            ),
        ]);
        // The healthy block still bakes; broken selection paths return None.
        assert_eq!(pack.model("minecraft:stone", &[]).quads.len(), 6);
        assert!(pack.pack.model(&pack.state("minecraft:air", &[])).is_none());
        assert!(
            pack.pack
                .model(&pack.state("minecraft:only", &[]))
                .is_none()
        );
        assert!(
            pack.pack
                .model(&pack.state("minecraft:ghost", &[]))
                .is_none()
        );
        assert!(
            pack.pack
                .model(&pack.state("minecraft:broken", &[]))
                .is_none()
        );
        assert!(
            pack.pack
                .model(&pack.state("minecraft:cycle", &[]))
                .is_none()
        );
        assert!(pack.pack.model(&pack.state("mod:stone", &[])).is_none());
    }

    #[test]
    fn repeated_model_requests_return_the_cached_bake() {
        let pack = Pack::new(STONE_PACK);
        assert!(pack.pack.cache.borrow().is_empty());

        let first = pack.pack.model(&pack.state("minecraft:stone", &[]));
        assert_eq!(pack.pack.cache.borrow().len(), 1);
        let second = pack.pack.model(&pack.state("minecraft:stone", &[]));
        let third = pack.pack.model(&pack.state("minecraft:stone", &[]));
        assert_eq!(first, second);
        assert_eq!(second, third);
        assert_eq!(first.as_ref().map(|model| model.quads.len()), Some(6));
        // Still a single entry: hits do not re-bake or duplicate.
        assert_eq!(pack.pack.cache.borrow().len(), 1);

        // A state with different properties is a distinct cache key.
        let other = pack.state("minecraft:stone", &[("axis", "y")]);
        let _ = pack.pack.model(&other);
        assert_eq!(pack.pack.cache.borrow().len(), 2);

        // Unsupported states are cached as `None` and stay `None`.
        let air = pack.state("minecraft:air", &[]);
        assert!(pack.pack.model(&air).is_none());
        assert!(pack.pack.model(&air).is_none());
        assert!(pack.pack.cache.borrow().contains_key(&air));
        assert_eq!(pack.pack.cache.borrow().len(), 3);

        // Baked models are still equal to an independently baked clone.
        let baked = first.expect("stone bakes");
        assert_eq!(baked.quads.len(), 6);
    }

    #[test]
    fn render_item_bakes_block_models_and_reports_item_layers() {
        let mut files = STONE_PACK.to_vec();
        files.push((
            "models/item/generated.json",
            r#"{"textures": {"layer0": "item/apple"}}"#,
        ));
        files.push(("models/item/apple.json", r#"{"parent": "item/generated"}"#));
        let pack = Pack::new(&files);

        // A name with a blockstate file bakes the block model.
        let stone = pack
            .pack
            .render_item("minecraft:stone")
            .expect("stone renders");
        assert_eq!(stone.model.quads.len(), 6);
        assert_eq!(stone.textures, vec!["minecraft:block/stone"]);
        assert_eq!(pack.pack.render_item("stone").as_ref(), Some(&stone));

        // A generated item model has no elements but exposes its sprite layer.
        let apple = pack
            .pack
            .render_item("minecraft:apple")
            .expect("apple renders");
        assert!(apple.model.quads.is_empty());
        assert_eq!(apple.textures, vec!["minecraft:item/apple"]);

        assert!(pack.pack.render_item("minecraft:unknown_item").is_none());
        assert!(pack.pack.render_item("").is_none());
    }
}

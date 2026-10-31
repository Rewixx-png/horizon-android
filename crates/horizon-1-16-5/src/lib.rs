//! Native Minecraft 1.16.5 data/frame foundation, separate from the 26.3
//! engine. World files, format-6 models and CPU chunk meshes live here so a
//! future 1.16.5 renderer can reuse them without touching 26.3 tables.

pub mod assets;
pub mod mesh;
pub mod preview;
mod types;
pub mod world;

pub use types::{
    BakedModel, BlockSource, BlockState, BlockTable, Face, Mesh, ModelSource, Properties, Quad,
    Vertex,
};

/// Protocol 754 primitives from the network workstream.
pub use horizon_protocol::v1_16_5 as protocol;

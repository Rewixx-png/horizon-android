//! Native Minecraft 1.16.5 data/frame foundation, separate from the 26.3
//! engine. World files, format-6 models, CPU chunk meshes, texture atlases
//! and an offscreen wgpu frame live here so a future 1.16.5 renderer can
//! reuse them without touching 26.3 tables.

#[cfg(target_os = "android")]
pub mod android_main;
pub mod app;
pub mod assets;
pub mod atlas;
pub mod blocks;
pub mod chunk_codec;
pub mod chunk_mesh;
pub mod crafting;
pub mod font;
pub mod gui;
pub mod interact;
pub mod inventory;
pub mod inventory_ui;
pub mod item_icons;
pub mod light;
pub mod loot;
pub mod mesh;
pub mod net;
pub mod particles;
pub mod player;
pub mod preview;
pub mod render;
pub mod server;
pub mod survival;
pub mod touch;
mod types;
pub mod world;
pub mod worldgen;

pub use loot::LootTables;
pub use types::{
    BakedModel, BlockSource, BlockState, BlockTable, Face, Mesh, ModelSource, Properties, Quad,
    Vertex,
};

/// Protocol 754 primitives from the network workstream.
pub use horizon_protocol::v1_16_5 as protocol;

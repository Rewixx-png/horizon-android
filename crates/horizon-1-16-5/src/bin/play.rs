//! The playable 1.16.5 desktop target.
//!
//! cargo run -p horizon-1-16-5 --bin horizon-1165 -- \
//!   --world .local/versions/1.16.5/world \
//!   --tables .local/versions/1.16.5/tables.json \
//!   --client-jar .local/versions/1.16.5/inputs/client.jar \
//!   --assets-index .local/versions/1.16.5/inputs/assets/indexes/1.16.json \
//!   --assets-objects .local/versions/1.16.5/inputs/assets/objects

fn main() {
    if let Err(error) = horizon_1_16_5::app::run() {
        eprintln!("horizon-1165: {error}");
        std::process::exit(1);
    }
}

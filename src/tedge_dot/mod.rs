//! tedge-dot interop: connect with the settings of a device in a tedge-dot OPC
//! UA connector config, and write discovered nodes as a tedge-dot point library.
//!
//! The formats are defined by tedge-dot (contract §3.4, OPC UA connector spec
//! §3); see `openspec/changes/add-tedge-dot-point-library/`.

pub mod connection;
pub mod library;
pub mod points;
pub mod schema;

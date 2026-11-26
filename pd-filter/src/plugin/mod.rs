//! Third-party plugins (protocol: `plugin-sdk/PROTOCOL.md`). `manifest` reads `plugin.json`, `rpc` runs the plugin process and its JSON lines, `adapter` makes
//! a plugin a `LibrarySource` like any built-in one.
pub mod adapter;
pub mod manifest;
pub mod rpc;

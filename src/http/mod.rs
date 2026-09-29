//! What the server exposes: the router, the REST and WebSocket routes, the
//! client protocol, and device auth and push.

pub mod api;
pub mod auth;
pub mod files;
pub mod git;
pub mod net;
pub mod push;
pub mod server;
pub mod wire;
pub mod ws;

//! Connection Handlers
//!
//! This module handles incoming and outgoing network connections:
//! - `incoming/`: Handlers for connections initiated by other nodes
//! - `outgoing/`: Methods for initiating connections to other nodes

pub(crate) mod incoming;
pub(crate) mod outgoing;


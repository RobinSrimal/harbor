//! Testing utilities for Harbor Protocol
//!
//! Provides an in-process simulation framework for testing without a real network.
//!
//! # Example
//!
//! ```ignore
//! let mut network = TestNetwork::new();
//!
//! // Create nodes
//! let alice = network.add_node();
//! let bob = network.add_node();
//!
//! // Bootstrap the DHT
//! network.bootstrap_dht();
//!
//! // Alice sends to bob
//! let topic = network.create_topic(&[alice, bob]);
//! network.send_packet(alice, &topic, b"Hello Bob!").await;
//!
//! // Bob receives
//! let packet = network.receive_packet(bob).await;
//! ```

pub mod network;
pub mod node;

pub use network::TestNetwork;
pub use node::TestNode;

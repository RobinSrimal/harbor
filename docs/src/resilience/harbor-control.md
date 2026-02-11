# Resilience: Harbor, DHT, Control

Resilience is about keeping the network usable under load or abuse. The **DHT**, **Harbor**, and **control plane** are critical infrastructure, so they use **stronger protections**.

## What Provides Resilience

- **Strong Proof of Work** on DHT, Harbor, and control-plane requests
- **Rate limits** per peer and per topic
- **Replication** of Harbor packets across multiple nodes
- **Storage caps** to prevent any one topic from consuming all space

## What This Means for You

Most apps don't need to tune these settings. The defaults are designed to keep the network healthy without hurting normal usage.

If you run infrastructure nodes, you can adjust storage limits, replication factor, and timing intervals through `ProtocolConfig`.

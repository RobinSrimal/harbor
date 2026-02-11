# Resilience: Data Plane

The data plane focuses on **user-level operations**. It uses lighter protection to keep things fast while still preventing abuse.

## What Provides Resilience

- **Gatekeeping**: data-plane traffic only flows between connected peers or topic members
- **Weak Proof of Work** for high-volume messaging
- **Backpressure** and request/response patterns for streams and file sharing

## What This Means for You

- Use the control plane to decide who is allowed to talk.
- Expect stream requests and file shares to be accepted explicitly.
- For high-volume apps, consider your own application-level rate limits.

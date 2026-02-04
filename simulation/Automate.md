# Simulation Automation Procedure

Post-refactor verification procedure for running all Harbor simulation scenarios sequentially.

## Procedure

1. Run each scenario individually using `./simulate.sh --scenario <name>`
2. Check logs for pass/fail
3. If **pass**: record result below, move to next scenario
4. If **fail**: stop, investigate, get human input before continuing

## Run Order

Scenarios are run in the order they appear in `simulate.sh` (source order):

### Basic
| # | Scenario | Command | Status |
|---|----------|---------|--------|
| 1 | basic | `./simulate.sh basic` | |
| 2 | offline | `./simulate.sh offline` | |
| 3 | dht-churn | `./simulate.sh dht-churn` | |

### Membership
| # | Scenario | Command | Status |
|---|----------|---------|--------|
| 4 | member-join | `./simulate.sh member-join` | |
| 5 | member-leave | `./simulate.sh member-leave` | |
| 6 | new-to-offline | `./simulate.sh new-to-offline` | |
| 7 | offline-sync | `./simulate.sh offline-sync` | |
| 8 | deduplication | `./simulate.sh deduplication` | |

### Advanced
| # | Scenario | Command | Status |
|---|----------|---------|--------|
| 9 | multi-topic | `./simulate.sh multi-topic` | |
| 10 | concurrent | `./simulate.sh concurrent` | |
| 11 | large-message | `./simulate.sh large-message` | |
| 12 | scale-members | `./simulate.sh scale-members` | |
| 13 | harbor-sync | `./simulate.sh harbor-sync` | |
| 14 | harbor-churn | `./simulate.sh harbor-churn` | |
| 15 | race-join | `./simulate.sh race-join` | |
| 16 | flap | `./simulate.sh flap` | |
| 17 | dht-pool | `./simulate.sh dht-pool` | |

### File Sharing
| # | Scenario | Command | Status |
|---|----------|---------|--------|
| 18 | share-basic | `./simulate.sh share-basic` | |
| 19 | share-peer-offline | `./simulate.sh share-peer-offline` | |
| 20 | share-few-peers | `./simulate.sh share-few-peers` | |
| 21 | share-large-file | `./simulate.sh share-large-file` | |
| 22 | share-retry-source | `./simulate.sh share-retry-source` | |
| 23 | share-retry-recipient | `./simulate.sh share-retry-recipient` | |

### CRDT Sync
| # | Scenario | Command | Status |
|---|----------|---------|--------|
| 24 | sync-basic | `./simulate.sh sync-basic` | |
| 25 | sync-concurrent | `./simulate.sh sync-concurrent` | |
| 26 | sync-offline | `./simulate.sh sync-offline` | |
| 27 | sync-initial | `./simulate.sh sync-initial` | |

### Stream
| # | Scenario | Command | Status |
|---|----------|---------|--------|
| 28 | stream-basic | `./simulate.sh stream-basic` | |
| 29 | stream-reject | `./simulate.sh stream-reject` | |
| 30 | stream-end | `./simulate.sh stream-end` | |

### Direct Messaging
| # | Scenario | Command | Status |
|---|----------|---------|--------|
| 31 | dm-basic | `./simulate.sh dm-basic` | |
| 32 | dm-offline | `./simulate.sh dm-offline` | |
| 33 | dm-sync-basic | `./simulate.sh dm-sync-basic` | |
| 34 | dm-sync-offline | `./simulate.sh dm-sync-offline` | |
| 35 | dm-share-basic | `./simulate.sh dm-share-basic` | |
| 36 | dm-share-offline | `./simulate.sh dm-share-offline` | |

### Control Protocol
| # | Scenario | Command | Status |
|---|----------|---------|--------|
| 37 | control-connect | `./simulate.sh control-connect` | |
| 38 | control-topic-invite | `./simulate.sh control-topic-invite` | |
| 39 | control-suggest | `./simulate.sh control-suggest` | |
| 40 | control-offline | `./simulate.sh control-offline` | |

## Notes

- Each scenario is self-contained (starts nodes, runs test, cleans up)
- Logs are in `/tmp/harbor-sim/` during runs
- On failure, check node logs at `/tmp/harbor-sim/node-*/harbor.log`

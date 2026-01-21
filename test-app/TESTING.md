# Testing Harbor with Multiple Instances

This guide explains how to run multiple test-app instances locally for testing collaborative features.

## Quick Start

### macOS / Linux

Open two terminals in the `test-app` directory:

**Terminal 1:**
```bash
./run-dev-instance1.sh
```

**Terminal 2:**
```bash
./run-dev-instance2.sh
```

### Windows

Open two command prompts in the `test-app` directory:

**Terminal 1:**
```cmd
run-dev-instance1.bat
```

**Terminal 2:**
```cmd
run-dev-instance2.bat
```

## Testing Collaborative Editing

1. **Start both instances** using the commands above

2. **In Instance 1:**
   - Click "Create Document"
   - Copy the invite code

3. **In Instance 2:**
   - Paste the invite code
   - Click "Join"

4. **Test collaboration:**
   - Type in either instance
   - Watch the text sync to the other instance in real-time

## Log Files

Each instance writes logs to its own directory:

- **Instance 1 logs:**
  - Backend: `test-data/instance1/app.log`
  - Frontend: `test-data/instance1/frontend.log`

- **Instance 2 logs:**
  - Backend: `test-data/instance2/app.log`
  - Frontend: `test-data/instance2/frontend.log`

### Viewing Logs

**Watch logs in real-time (macOS/Linux):**
```bash
# Watch instance 1 logs
tail -f test-data/instance1/app.log test-data/instance1/frontend.log

# Watch instance 2 logs
tail -f test-data/instance2/app.log test-data/instance2/frontend.log
```

**Windows:**
```cmd
# View logs
type test-data\instance1\frontend.log
type test-data\instance2\frontend.log
```

## Database Files

Each instance uses its own database:

- Instance 1: `test-data/instance1/harbor.db`
- Instance 2: `test-data/instance2/harbor.db`

### Start Fresh

To delete all test data and start with new identities, use the cleanup script:

**macOS/Linux:**
```bash
./clean-test-data.sh
```

**Windows:**
```cmd
clean-test-data.bat
```

**IMPORTANT:** If both instances show the same endpoint ID in their logs, you MUST run the cleanup script. This ensures each instance gets a unique identity.

## Troubleshooting

### Both instances have the same endpoint ID

**Problem:** If you see the same endpoint ID in both instance logs, Harbor thinks they're the same peer.

**Solution:** Run the cleanup script to give each instance a unique identity:
```bash
./clean-test-data.sh  # macOS/Linux
clean-test-data.bat   # Windows
```

Then restart both instances.

**How to check:** Look for "Protocol started endpoint_id=" in the backend logs. Each instance should show a different ID.

### Instances won't connect to each other

1. Check that both instances are using the same bootstrap node (they should by default)
2. Verify each instance has a different endpoint ID (see above)
3. Check the backend logs for connection errors
4. Make sure both instances have started successfully

### Changes not syncing

1. First, verify both instances have different endpoint IDs (see above)
2. Open browser DevTools (F12) and check the Console tab for errors
3. Check the frontend logs in `test-data/instance*/frontend.log`
4. Verify both instances joined the same topic (check the topic IDs match)
5. When Instance 2 joins, it should see TWO members, not just one

### Port conflicts

If you get a port conflict error, one of these might help:
- Kill any existing test-app processes
- Restart your computer to free up ports
- Check if another app is using port 1420

## Architecture Notes

- Each instance has a separate identity (endpoint ID)
- Instances communicate via the Harbor protocol over QUIC
- Sync uses the Loro CRDT for collaborative editing
- Offline changes are stored in Harbor nodes for later delivery

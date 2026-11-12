#!/usr/bin/env bash
# Build the release binary and copy it to the server (only into $REMOTE_DIR; nothing is installed system-wide).
. "$(dirname "$0")/lib.sh"
cd "$PROJECT_DIR" && cargo build --release -p periwire 2>&1 | tail -1
remote "mkdir -p '$REMOTE_DIR'"
scp "${SSH_OPTS[@]}" -q target/release/periwire "$REMOTE_USER@$REMOTE_HOST:$REMOTE_DIR/periwire.new"
remote "mv -f '$REMOTE_DIR/periwire.new' '$REMOTE_DIR/periwire' && chmod 755 '$REMOTE_DIR/periwire' && '$REMOTE_DIR/periwire' --help | head -1"

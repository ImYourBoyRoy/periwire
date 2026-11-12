#!/usr/bin/env bash
# Cross-compile periwire.exe on the lab server (mingw-w64 + the windows-gnu Rust target) and fetch it to dist/windows/.
# Source goes to $REMOTE_DIR/src only; nothing else on the server is touched.
. "$(dirname "$0")/lib.sh"
need_lab
SRC="$REMOTE_DIR/src"
remote "mkdir -p '$SRC'"
rsync -a --delete -e "ssh ${SSH_OPTS[*]}" --exclude target --exclude .git --exclude .state --exclude private --exclude .env --exclude 'tools/lab.env' --exclude dist --exclude node_modules \
  "$PROJECT_DIR/" "$REMOTE_USER@$REMOTE_HOST:$SRC/" || exit 1
remote "export PATH=\$HOME/.cargo/bin:\$PATH; cd '$SRC' && cargo build --release --target x86_64-pc-windows-gnu -p periwire 2>&1 | tail -${TAIL:-15}" || exit 1
mkdir -p "$PROJECT_DIR/dist/windows"
scp "${SSH_OPTS[@]}" -q "$REMOTE_USER@$REMOTE_HOST:$SRC/target/x86_64-pc-windows-gnu/release/periwire.exe" "$PROJECT_DIR/dist/windows/" && ls -la "$PROJECT_DIR/dist/windows/periwire.exe"

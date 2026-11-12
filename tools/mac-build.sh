#!/usr/bin/env bash
# Sync the project to the macOS guest (~/periwire only) and build + test it there. Never touches other
# guest directories, keychains or services.   Usage: tools/mac-build.sh [test|build|release]
. "$(dirname "$0")/lib.sh"
GDIR="/Users/$REMOTE_USER/periwire"
guest "mkdir -p '$GDIR'"
# rsync through the jump host; exclude build output, state and secrets
rsync -a --delete --exclude target --exclude .state --exclude .env --exclude .git \
  -e "ssh ${SSH_OPTS[*]} -J $REMOTE_USER@$REMOTE_HOST" \
  "$PROJECT_DIR/" "$REMOTE_USER@$GUEST_HOST:$GDIR/" || exit 1
case "${1:-test}" in
  test)    guest "cd '$GDIR' && export PATH=\$HOME/.cargo/bin:\$PATH && cargo test 2>&1 | tail -40" ;;
  build)   guest "cd '$GDIR' && export PATH=\$HOME/.cargo/bin:\$PATH && cargo build 2>&1 | tail -60" ;;
  release) guest "cd '$GDIR' && export PATH=\$HOME/.cargo/bin:\$PATH && cargo build --release 2>&1 | tail -30 && ls -la target/release/periwire" ;;
esac

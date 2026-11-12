#!/usr/bin/env bash
# Shared helpers for USBForwarder tools. Source this; do not run it.
# Secrets policy: .env is parsed key-by-key (never sourced/eval'd), values are never
# printed, and the sudo password goes to sudo via stdin, never on argv.

TOOLS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$TOOLS_DIR")"
ENV_FILE="$PROJECT_DIR/.env"

env_get() {  # env_get KEY -> value on stdout (empty if missing)
  [ -f "$ENV_FILE" ] || return 0
  grep -m1 -E "^$1=" "$ENV_FILE" | cut -d= -f2- | sed -e 's/^"//' -e 's/"$//' -e "s/^'//" -e "s/'$//"
}

# run_sudo CMD [ARGS...] : run a command as root using the .env password via stdin.
run_sudo() {
  local pw; pw="$(env_get sudo_password)"
  if [ -z "$pw" ]; then echo "sudo_password missing in .env" >&2; return 1; fi
  printf '%s\n' "$pw" | sudo -S -p '' "$@"
}

hr() { printf '\n== %s ==\n' "$*"; }


# ---- remote (server) helpers ----
# Lab settings live in tools/lab.env (git-ignored; copy tools/lab.env.example).
LAB_ENV="$TOOLS_DIR/lab.env"
# shellcheck disable=SC1090
[ -f "$LAB_ENV" ] && . "$LAB_ENV"
REMOTE_HOST="${PERIWIRE_REMOTE_HOST:-${REMOTE_HOST:-}}"   # PERIWIRE_REMOTE_HOST overrides lab.env (e.g. a Tailscale address when away from the LAN)
REMOTE_USER="${REMOTE_USER:-$(env_get username)}"
REMOTE_DIR="${REMOTE_DIR:-/home/$REMOTE_USER/periwire}"   # everything for this project lives here
SSH_OPTS=(-i "$HOME/.ssh/id_ed25519" -o BatchMode=yes -o ConnectTimeout=8)

need_lab() { [ -n "$REMOTE_HOST" ] || { echo "REMOTE_HOST not set: copy tools/lab.env.example to tools/lab.env" >&2; exit 1; }; }
remote() {  # remote CMD...  : run as the normal user on the server
  need_lab
  ssh "${SSH_OPTS[@]}" "$REMOTE_USER@$REMOTE_HOST" "$@"
}
remote_sudo() {  # remote_sudo 'shell command' : run as root on the server (password via stdin)
  need_lab
  local pw; pw="$(env_get sudo_password)"
  printf '%s\n' "$pw" | ssh "${SSH_OPTS[@]}" "$REMOTE_USER@$REMOTE_HOST" "sudo -S -p '' sh -c $(printf '%q' "$1")"
}

# ---- macOS guest (reached through the server; read-only helper) ----
GUEST_HOST="${PERIWIRE_GUEST_HOST:-${GUEST_HOST:-}}"   # PERIWIRE_GUEST_HOST overrides lab.env (e.g. a throwaway test VM)
guest() {  # guest CMD... : run as the normal user on the macOS guest (via ssh jump through the server)
  need_lab
  ssh "${SSH_OPTS[@]}" -J "$REMOTE_USER@$REMOTE_HOST" "$REMOTE_USER@$GUEST_HOST" "$@"
}

# need_spec [arg]: the device to test with, as vid:pid (the first argument, or PERIWIRE_TEST_DEVICE). Sets SPEC. There is no default on purpose:
# use a SPARE device (`periwire devices` or `lsusb` lists them), never one with data you care about.
need_spec() {
  SPEC="${1:-${PERIWIRE_TEST_DEVICE:-}}"
  [ -n "$SPEC" ] || { echo "ABORT: say which device to test with: vid:pid as the argument, or set PERIWIRE_TEST_DEVICE (see 'periwire devices' or 'lsusb'). Use a spare device." >&2; exit 1; }
}

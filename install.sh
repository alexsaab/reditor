#!/usr/bin/env sh
set -eu

REPO_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
cd "$REPO_DIR"

echo "==> Installing reditor binary via Cargo..."
cargo install --path . --locked --force

BIN_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"
REDITOR_BIN="$BIN_DIR/reditor"

if [ ! -x "$REDITOR_BIN" ]; then
    REDITOR_BIN="$(command -v reditor)"
fi

echo "==> Installing and activating all bundled plugins in ~/.reditor/plugins..."
"$REDITOR_BIN" --install-all-plugins --scope user

echo ""
echo "✓ reditor is installed: $REDITOR_BIN"
echo "✓ All plugins activated in ~/.reditor/plugins"
case ":$PATH:" in
    *":$BIN_DIR:"*)
        echo "✓ You can now run 'reditor' or 'reditor .' from any directory!"
        ;;
    *)
        echo "! Note: Add $BIN_DIR to your PATH:"
        echo "  export PATH=\"$BIN_DIR:\$PATH\""
        ;;
esac

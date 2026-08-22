#!/bin/bash
set -euo pipefail

SOURCE="${1:-}"
APP_NAME="DSH Thin Desktop.app"
DESTINATION="/Applications/$APP_NAME"
MOUNT_POINT=""

cleanup() {
  if [ -n "$MOUNT_POINT" ]; then
    hdiutil detach "$MOUNT_POINT" -quiet || true
    rmdir "$MOUNT_POINT" 2>/dev/null || true
  fi
}
trap cleanup EXIT

if [ -z "$SOURCE" ]; then
  SOURCE="$(find "$HOME/Downloads" -maxdepth 1 -name 'DSH.Thin.Desktop_*.dmg' -print | sort -r | head -1)"
fi

if [ -z "$SOURCE" ] || [ ! -e "$SOURCE" ]; then
  echo "未找到安装包，请传入 .dmg 或 .app 路径" >&2
  exit 1
fi

if [ -d "$SOURCE" ] && [ "$(basename "$SOURCE")" = "$APP_NAME" ]; then
  APP_PATH="$SOURCE"
elif [ -f "$SOURCE" ] && [ "${SOURCE##*.}" = "dmg" ]; then
  MOUNT_POINT="$(mktemp -d)"
  hdiutil attach "$SOURCE" -readonly -nobrowse -mountpoint "$MOUNT_POINT" -quiet
  APP_PATH="$MOUNT_POINT/$APP_NAME"
else
  echo "仅支持 DSH Thin Desktop 的 .dmg 或 .app" >&2
  exit 1
fi

if [ ! -d "$APP_PATH" ]; then
  echo "安装包中未找到 $APP_NAME" >&2
  exit 1
fi

rm -rf "$DESTINATION"
ditto "$APP_PATH" "$DESTINATION"
xattr -dr com.apple.quarantine "$DESTINATION"
open "$DESTINATION"

echo "已安装到 $DESTINATION"

# Probe npm / pnpm / bun for their global binary directories.
# Sets these variables for downstream scripts to read:
#   _remora_npm_global_bin
#   _remora_pnpm_global_bin
#   _remora_bun_global_bin
# Requires PROFILE_INIT to have run first.
_remora_npm_prefix=""
_remora_npm_global_bin=""
_remora_pnpm_global_bin=""
_remora_bun_global_bin=""
if command -v npm >/dev/null 2>&1; then
  _remora_npm_prefix="$(npm config get prefix 2>/dev/null || true)"
  case "$_remora_npm_prefix" in
    "" | "undefined" | "null")
      _remora_npm_prefix=""
      ;;
    *)
      _remora_npm_global_bin="$_remora_npm_prefix/bin"
      ;;
  esac
fi
if command -v pnpm >/dev/null 2>&1; then
  _remora_pnpm_global_bin="$(pnpm bin -g 2>/dev/null || true)"
fi
if command -v bun >/dev/null 2>&1; then
  _remora_bun_global_bin="$(bun pm bin -g 2>/dev/null || true)"
fi

# Source common shell rc files into PATH so user-installed binaries (npm,
# pnpm, bun, codex) become reachable from `/bin/sh`. We run each rc in a
# subshell so per-shell-only syntax (e.g. zsh-isms) cannot crash the parent
# /bin/sh, then re-import the resulting PATH via a temp file.
_remora_path_prepend() { case ":$PATH:" in *":$1:"*) ;; *) [ -d "$1" ] && PATH="$1:$PATH" ;; esac; }
_remora_pf="/tmp/.remora_path_$$"; for f in "$HOME/.zshenv" "$HOME/.profile" "$HOME/.bash_profile" "$HOME/.bashrc" "$HOME/.zprofile" "$HOME/.zshrc"; do [ -f "$f" ] && (. "$f" 2>/dev/null; echo "$PATH") > "$_remora_pf" 2>/dev/null && PATH="$(cat "$_remora_pf")" ; done; rm -f "$_remora_pf" 2>/dev/null;
_remora_path_prepend "$NVM_BIN"; _remora_path_prepend "${ASDF_DATA_DIR:-}/shims"; _remora_path_prepend "/opt/homebrew/opt/node/bin"; _remora_path_prepend "/opt/homebrew/bin"; _remora_path_prepend "/usr/local/opt/node/bin"; _remora_path_prepend "/usr/local/bin"; _remora_path_prepend "$HOME/.volta/bin"; _remora_path_prepend "$HOME/.bun/bin"; _remora_path_prepend "$HOME/.local/bin"; _remora_path_prepend "${CARGO_HOME:-$HOME/.cargo}/bin"; _remora_path_prepend "${PNPM_HOME:-$HOME/Library/pnpm}"; _remora_path_prepend "$HOME/.opencode/bin";
_remora_nvm_dir="${NVM_DIR:-$HOME/.nvm}"; if [ -d "$_remora_nvm_dir/versions/node" ]; then _remora_nvm_default=""; [ -f "$_remora_nvm_dir/alias/default" ] && _remora_nvm_default="$(cat "$_remora_nvm_dir/alias/default" 2>/dev/null || true)"; [ -n "$_remora_nvm_default" ] && _remora_path_prepend "$_remora_nvm_dir/versions/node/$_remora_nvm_default/bin"; for d in "$_remora_nvm_dir"/versions/node/*/bin; do [ -x "$d/node" ] && _remora_path_prepend "$d"; done; fi;
if [ -d "$HOME/.fnm/node-versions" ]; then for d in "$HOME"/.fnm/node-versions/*/installation/bin; do [ -x "$d/node" ] && _remora_path_prepend "$d"; done; fi;
_remora_path_prepend "$HOME/.asdf/shims"; _remora_path_prepend "$HOME/.local/share/mise/shims";
export PATH;

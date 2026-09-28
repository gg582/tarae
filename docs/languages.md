# Language support

Syntax highlighting covers 54 languages ([how grammars work](features.md#syntax-highlighting)). Everything beyond
colors comes from external tools that tarae finds on your `PATH` — install the ones for the languages you use.

| Language | Language server (first one found) | Tests | Debugger |
|---|---|---|---|
| Rust | `rust-analyzer` | `cargo test` | `lldb-dap` (builds with `cargo build` first) |
| C / C++ | `clangd` | — | `lldb-dap` |
| Go | `gopls` | `go test` | `dlv` |
| Python | `basedpyright`, `pyright`, `pylsp` | `pytest` | `debugpy` |
| Java | `jdtls` (Java 21+) | Gradle (`./gradlew` or `gradle`) · Maven (`./mvnw` or `mvn`) | java-debug inside jdtls (offered for download) |
| TypeScript / JavaScript (incl. TSX, JSX) | `typescript-language-server` | — | — |
| Bash | `bash-language-server` | — | — |
| Lua | `lua-language-server` | — | — |
| Zig | `zls` | — | — |
| Nix | `nil` | — | — |
| TOML | `taplo` | — | — |
| YAML | `yaml-language-server` | — | — |
| Helm templates | `helm_ls` | — | — |
| Markdown | `marksman` | — | — |

Any other server can be added, or the choice changed, in your config — see
[Language servers](configuration.md#language-servers).

Notes:

- **lldb-dap** is found as `lldb-dap`, `lldb-vscode`, or a versioned name like `lldb-dap-18` (as Linux packages install
  it); on macOS, through `xcrun` as well. macOS needs a one-time `sudo DevToolsSecurity -enable`
- **dlv** must be newer than your Go (`go install github.com/go-delve/delve/cmd/dlv@latest`)
- **debugpy** and **pytest** run with the project's `.venv` Python, or `$VIRTUAL_ENV`, when there is one
- **java-debug** is a jdtls extension that jdtls doesn't ship. `F5` offers to download it from Maven Central (with
  `curl` or `wget`); copies installed by VS Code or Neovim's mason are found too

## Other tools tarae uses

| Tool | For | Without it |
|---|---|---|
| `git` + a C compiler | Downloading and building grammars on first open | Build with `--features bundled-grammars` for 26 grammars built in |
| `git` | File picker in a repository (`git ls-files`, respects `.gitignore`) | A plain directory walk |
| `rg` (ripgrep) | Global search `space /` | A built-in regex search |
| `claude` ([Claude Code](https://claude.com/claude-code)) | [Claude features](claude-integration.md) | Everything else works |
| zellij or tmux | `space c` — Claude Code in a side pane | Run `claude` yourself and connect with `/ide` |
| `pbcopy` (macOS) · `wl-copy` / `xclip` / `xsel` (Linux) · `win32yank` or PowerShell (WSL) | The `+` register — `space y`, `space p` | Clipboard commands report an error |

# Tests and debugging

Run the test under your cursor, see failures on the line that failed, and step through code — for Rust, Go, Python,
and Java (plus C and C++ in the debugger). Which tools each language needs: [Language support](languages.md).

- [Tests](#tests)
- [Debugger](#debugger)
- [Attaching to a running program](#attaching-to-a-running-program)

## Tests

`space x`, then:

| Key | Does |
|---|---|
| `x` | Run the test at the cursor |
| `d` | Debug the test at the cursor |
| `f` | Run this file's tests |
| `l` | Run the last test again (as a debug run if it was one) |
| `c` | Close the panel (stops a run that's still going) |

| Language | Runs with |
|---|---|
| Rust | `cargo test -- module::path::name --exact` (integration tests with `--test`) |
| Go | `go test -v -run '^TestX$'` (benchmarks with `-bench`) |
| Python | `pytest file::Class::function`, with the project's `.venv` or `$VIRTUAL_ENV` Python |
| Java | Gradle `./gradlew :module:test --tests pkg.Class.method` (nested classes as `Outer$Inner`) or Maven `./mvnw -pl module -Dtest=Class#method test` — JUnit 4/5 and TestNG |

**Finding tests** works on the tree-sitter tree — `#[test]`-style attributes and `mod` paths in Rust, `TestXxx` in Go,
`class Test…`/`def test…` in Python, `@Test`-style annotations in Java. If the cursor isn't inside a test, the enclosing
`mod tests` or test class runs; failing that, the whole file. If the language's grammar isn't installed yet, tarae asks
once — one `y` downloads it and runs the test right away ([screenshot](screenshots/grammar-needed.png)). The same goes
for `F5` and attach.

Modified files are saved before a run. **Results** fill a panel where the debug panel sits: a header like
`FAILED  math::*   ● 1  ▲ 1   26 ms`, each test on the left as `●` passed · `▲` failed · `◦` skipped (shapes, not just
colors), and the selected failure's message and location on the right. The failing line gets a red `▲ message` at its
end; `]t` `[t` jump between failures.

Results are read from libtest output, `go test -v`, `pytest -v --tb=short`, and for Java the Gradle/Maven JUnit XML
reports. While results can't be read yet (still building, a compile error), the full output shows instead.

[Rust](screenshots/test-results-rust.png) · [Go](screenshots/test-results-go.png) ·
[Python](screenshots/test-results-python.png) · [Java](screenshots/test-results-java.png) ·
[run output](screenshots/test-run.png) · [Java build output](screenshots/test-run-java.png)

**Debugging a test** (`space x d`) runs just that one test under the debugger — Rust: the test binary from
`cargo test --no-run` under lldb-dap · Go: dlv in test mode · Python: pytest under debugpy · Java: the build tool starts
the test JVM waiting for a debugger (`--debug-jvm` / `-Dmaven.surefire.debug`) and tarae attaches on its own. Afterwards
`F5` runs the same test again ([screenshot](screenshots/test-debug.png) · [Java](screenshots/test-debug-java.png)).

## Debugger

DAP for Rust, C, and C++ (lldb-dap — Rust is built with `cargo build` first), Go (dlv), Python (debugpy), and Java
(java-debug inside jdtls — the current file's `main`, otherwise the project's first; Maven, Gradle, or a folder with no
build file). Everything lives under `space g`.

| Key | Does |
|---|---|
| `F9` / `space g b` | Toggle breakpoint (a red `●` before the line number) |
| `F5` / `space g l` | Start, or continue |
| `F10` / `space g n` | Step over |
| `F11` / `space g i` | Step in |
| `F12` / `space g o` | Step out |
| `space g p` | Pause |
| `space g t` | Stop (or detach, when attached) |
| `space g C-c` | Conditional breakpoint — `break when: step == 3` |
| `space g C-l` | Logpoint — prints `a is {a}` to the output without stopping |
| `space g w` / `space g W` | Add / remove a watch expression (`:watch expr`, `:unwatch`) |

When paused, the line gets a `▶` and a band, **the values of the variables used up to that line appear at its end**
(`a = 2  b = 3`), and the debug panel below shows the state (RUNNING · PAUSED · EXITED), variables with types, the call
stack (your frames crisp, library frames dimmed), and program output (stderr in red).
[Rust](screenshots/ux-debugger.png) · [Python](screenshots/ux-debugger-python.png) ·
[Go](screenshots/ux-debugger-go.png) · [Java](screenshots/ux-debugger-java.png)

A breakpoint with a condition turns orange, a logpoint turns accent, and the condition shows dimmed at the end of the
line. If the adapter rejects it (a bad expression), the reason appears right there in red. Watch expressions are
evaluated in the current frame on every pause and listed under WATCH at the top of the panel
([screenshot](screenshots/ux-debugger-conditions.png)).

- **macOS**: lldb needs a one-time `sudo DevToolsSecurity -enable` (or approving the password prompt). If it can't start
  within 8 seconds, tarae tells you
- **Go**: dlv must be newer than the installed Go. If it's out of date, the notice shows dlv's own explanation
  (`go install github.com/go-delve/delve/cmd/dlv@latest`)
- **Java**: the java-debug extension is offered for download the first time — see [Java](features.md#java)

## Attaching to a running program

`space g a` opens a picker of targets, or `:attach name` / `:attach host:port`:

| Language | Start the program with | tarae connects via |
|---|---|---|
| Python | `python -m debugpy --listen 5678 app.py` | TCP, straight to debugpy |
| Go | `dlv … --headless --listen :2345 --accept-multiclient` | TCP, straight to dlv |
| Java | `-agentlib:jdwp=transport=dt_socket,server=y,address=*:5005` | JDWP, through java-debug in jdtls |
| Rust / C | `gdbserver`, `lldb-server gdbserver`, or `debugserver` — or a local process by `pid` | lldb-dap |

In an attached session `space g t` **detaches** and leaves the program running. Give the targets you use often a name in
your config (a project `.tarae.toml` works well for this):

```toml
[[attach]]
name = "api (k8s)"
lang = "java"                                                # defaults to the current file's language
port = 5005                                                  # host defaults to 127.0.0.1 · for a local process, pid = …
before = "kubectl -n app port-forward deploy/api 5005:5005"  # started first, stopped with the session
remote-root = "/app"                                         # source root in the container ↔ this project
# program = "target/debug/app"                               # Rust/C: the executable to read symbols from
```

`before` runs a helper first (a port-forward, an ssh tunnel); tarae attaches once it prints a line or after 2 seconds,
and stops it when the session ends. `remote-root` maps breakpoint paths when the program was built somewhere else —
without it, breakpoints set in your copy won't match the paths the remote recorded.

[Python](screenshots/attach-python.png) · [Go](screenshots/attach-go.png) ·
[Java](screenshots/attach-java.png) · [Rust](screenshots/attach-rust.png)

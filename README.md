# PlugPort for MandaGram

Rust port by [@liznulo](https://t.me/liznulo). Based on CactusLib Mini for exteraGram by [@CactusPlugins](https://t.me/CactusPlugins).

Place `plugport.dll` (Windows x64) or `plugport.so` (Linux x86_64) in the `plugins/` folder next to the portable client. Enable PlugPort under Settings → Plugins.

- `.plf <id>` sends an installed plugin as a document in the current chat. The ID is the filename without its extension. The document keeps the original filename.
- `.chelp` sends a collapsible list of installed plugins with their versions, IDs, enabled status, and totals. Names and versions come from the loader's metadata when available.
- Use the gear icon in Settings → Plugins to rename either command. Each command must be a distinct single word starting with a dot.

To build, use Rust with edition 2024 support and run `cargo build --locked --release` from this folder on the target system. Use `plugport.dll` on Windows. On Linux, rename `libplugport.so` to `plugport.so`.

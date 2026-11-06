# TRAE session link experiment

This macOS demo converts a TRAE conversation ID or `context://history/<ID>`
into an OSC 8 link. A minimal VS Code extension receives the external URI and
calls TRAE's internal `_workbench.openRuntimeWindow` command to hand the
existing conversation to the **Agent** window (the current task/sidebar UI).
It does not send messages or create conversations.

From the session-finder repository:

```sh
python3 scripts/trae-session-link.py --install
python3 scripts/trae-session-link.py context://history/<24-digit-conversation-id>
python3 scripts/trae-session-link.py <24-digit-conversation-id> --open
python3 scripts/trae-session-link.py <24-digit-conversation-id> \
  --project-dir /path/to/conversation/project --open
```

The first open can display TRAE's URI permission dialog. Review the extension
name and select **Open** to continue. No persistent trust setting is required.

The wrapper resolves the exact conversation's working directory with the local
session-finder executable. Use `--finder` for another executable or
`--project-dir` to supply the directory explicitly. The project must exist
locally, and TRAE must be signed in to the account owning the conversation.

This is experimental and was tested against TRAE CN 3.4.15 on macOS.
Installation, URI delivery, and navigation to an existing conversation in the
Agent window were verified by checking its title and visible history.
The URI first reaches an IDE extension host, which may briefly display an IDE
window before completing the handoff. After updating the extension, run
**Developer: Reload Window** in that IDE window if it still uses the old bridge.
This demo supports local `chat-core` conversations; classic IDE-format and SSH
conversations are not covered. The internal API may change between releases.

The command result is logged to **Session Finder Link Demo** in the Output
panel as a boolean commit result. The demo does not log transcripts or
authentication information. Failed or unavailable handoffs are reported rather
than treating application activation as successful conversation navigation.

To remove the extension:

```sh
"/Applications/Trae CN.app/Contents/Resources/app/bin/trae-cn" \
  --uninstall-extension session-finder.trae-session-link
```

# Regenerates docs/demo.gif: capture a Claude Code setup, apply it to Cursor,
# confirm the plan, then undo. Recorded against a sandbox home; no real secrets.
# Run docs/demo/record.sh from a kitbag checkout (see that script).

Output demo.gif

Require kitbag

Set Shell "bash"
Set FontFamily "DejaVu Sans Mono"
Set FontSize 14
Set Width 1440
Set Height 700
Set Padding 16
Set Theme "Catppuccin Mocha"
Set WindowBar Rings
Set WindowBarSize 32
Set TypingSpeed 32ms
Set Framerate 20

Hide
Type "source /home/demo/bin/setup.sh"
Enter
Sleep 300ms
Ctrl+L
Show

Sleep 500ms

Type "kitbag capture --from claude-code -o ~/dotagents"
Sleep 350ms
Enter
Sleep 2500ms

Type "kitbag apply ~/dotagents --to cursor"
Sleep 300ms
Enter
Sleep 2800ms

Type "y"
Sleep 200ms
Enter
Sleep 1800ms

Type "kitbag undo"
Sleep 300ms
Enter
Sleep 3800ms

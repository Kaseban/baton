# baton demo — regenerate with: vhs assets/demo.tape
Output assets/demo.gif

Set FontSize 20
Set Width 1200
Set Height 520
Set Padding 24
Set Margin 20
Set MarginFill "#6C40D8"
Set BorderRadius 12
Set WindowBar Colorful
Set Theme "Catppuccin Mocha"
Set TypingSpeed 45ms

Hide
Type "cd ~/demo && rm -f handoff.json && clear"
Enter
Sleep 500ms
Type `clear && printf ' \e[1;95mbaton\e[0m \e[2m—\e[0m pass coding-agent sessions between tools\n'`
Enter
Sleep 300ms
Show

Type "# Hit your Claude Code usage limit mid-task?"
Sleep 800ms
Enter
Sleep 400ms

Type "baton convert --from claude-code --to opencode \"
Enter
Type "  session.jsonl -o handoff.json --import"
Sleep 900ms
Enter
Sleep 8s

Type "# session handed off — keep working in opencode ⚡"
Sleep 800ms
Enter
Sleep 2500ms

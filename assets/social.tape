# social preview frame — regenerate with: vhs assets/social.tape
Output assets/social-tmp.gif

Set FontSize 20
Set Width 1280
Set Height 640
Set Padding 32
Set Margin 24
Set MarginFill "#6C40D8"
Set BorderRadius 12
Set WindowBar Colorful
Set Theme "Catppuccin Mocha"
Set TypingSpeed 1ms

Hide
Type "cd ~/demo && rm -f handoff.json && clear"
Enter
Sleep 500ms
Type `clear && printf '\n\n\n\n \e[1;95mbaton\e[0m \e[2m—\e[0m pass coding-agent sessions between tools\n\n \e[36mClaude Code\e[0m \e[95m⇄\e[0m \e[36mopencode\e[0m \e[95m⇄\e[0m \e[36mCodex\e[0m \e[95m⇄\e[0m \e[36mGemini CLI\e[0m \e[95m⇄\e[0m \e[36mCursor\e[0m \e[95m⇄\e[0m \e[36mZed\e[0m \e[95m⇄\e[0m \e[36mAider\e[0m \e[95m⇄\e[0m \e[36mCline\e[0m\n\n \e[2mHit your usage limit mid-task? Hand off the session. Keep going.\e[0m\n'`
Enter
Sleep 500ms
Show

Type "baton convert --from claude-code --to opencode session.jsonl -o handoff.json --import"
Enter
Sleep 8s
Screenshot assets/social.png
Sleep 500ms

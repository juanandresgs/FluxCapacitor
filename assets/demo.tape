Output assets/demo.gif
Output assets/demo.mp4

Require flux
Require git

Set Shell "zsh"
Set FontFamily "SF Mono"
Set FontSize 16
Set LetterSpacing "-2"
Set LineHeight 1.0
Set Width 1520
Set Height 900
Set Padding 10
Set Margin 8
Set MarginFill "#0d0d10"
Set BorderRadius 8
Set WindowBar "Colorful"
Set WindowBarSize 30
Set Theme {"background":"#121215","foreground":"#d8d8dc","cursor":"#74d69a","black":"#121215","red":"#e07474","green":"#74d69a","yellow":"#e2b468","blue":"#75a4e8","magenta":"#c48fff","cyan":"#68beff","white":"#eeeeef","brightBlack":"#707078","brightRed":"#ff9191","brightGreen":"#93edaf","brightYellow":"#f4ca85","brightBlue":"#91b8f4","brightMagenta":"#dca3ff","brightCyan":"#8ad2ff","brightWhite":"#ffffff"}
Set Framerate 24
Set TypingSpeed 45ms

Hide
Type "unset NO_COLOR"
Enter
Type "./scripts/demo-session.sh prepare"
Enter
Sleep 400ms
Type "clear"
Enter
Show

Type "flux /tmp/flux-demo/storefront"
Enter
Sleep 250ms
Type "6"

Sleep 3.2s
Sleep 2.5s
Sleep 2.5s
Sleep 2.5s
Sleep 1.8s

Hide
Type "q"
Sleep 150ms
Type "./scripts/demo-session.sh cleanup"
Enter
Sleep 200ms

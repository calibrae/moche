#!/bin/bash
# Terminal feature toggles, one per ~1.5 s so each lands in its own frame.
s() { sleep 1.5; }
printf '\e[?1000h'; s
printf '\e[?1006h'; s
printf '\e[?1000l\e[?1006l'; s
printf '\e[?2004h'; s
printf '\e[?2004l'; s
printf '\a'; s
printf '\e]0;moche title zero\a'; s
printf '\e]2;moche title two\e\\'; s
printf '\e[?1049h\e[2J\e[5;10Halt screen\e[?1049l'; s
printf '\e[?25l'; s
printf '\e[?25h'; s
printf '\e[1;4;7;9mattrs\e[0m \e[3mital\e[0m \e[38;2;10;200;30mtruecolor\e[0m\n'

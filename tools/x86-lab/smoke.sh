#!/bin/bash
# Smoke test run on the lab host: moche --direct (echo, vim, resize, quit, --stats) and the real ssh bootstrap, inside guest b.
# Needs ~/moche-lab/bin/new/moche in b and a throwaway ssh key b -> a (README step 5).
L=${LAB:-$HOME/moche-lab2}/lab.sh
read -r P K < <($L ssh a "pkill mosh-server; SSH_CONNECTION='192.0.2.2 1 192.0.2.1 22' mosh-server new -s -c 256 -l LANG=C.UTF-8 2>&1 | sed -n 's/^MOSH CONNECT //p'" </dev/null)
$L ssh b "
tmux new -d -s s -x 100 -y 30 'env MOSH_KEY=$K ~/moche-lab/bin/new/moche --stats --direct 192.0.2.1 $P 2>/tmp/smoke.err'
sleep 3; tmux send-keys -t s 'echo direct-ok; tput cols; tput lines' Enter; sleep 1; tmux capture-pane -p -t s | head -6
tmux send-keys -t s 'vim /tmp/v.txt' Enter; sleep 1.5; tmux send-keys -t s 'ihello from vim' Escape; sleep 1
tmux capture-pane -p -t s | grep -n 'hello from vim'; tmux send-keys -t s ':q!' Enter; sleep 1
tmux resize-window -t s -x 90 -y 26; sleep 2; tmux send-keys -t s 'clear; tput cols; tput lines' Enter; sleep 1.5; tmux capture-pane -p -t s | head -4
tmux send-keys -t s C-^ .; sleep 1.5; cat /tmp/smoke.err; tmux kill-session -t s 2>/dev/null; true" </dev/null
echo '=== real ssh bootstrap'
$L ssh a 'pkill mosh-server; true' </dev/null
$L ssh b "
tmux new -d -s s2 -x 100 -y 30 'cd ~; ~/moche-lab/bin/new/moche --stats lab@192.0.2.1 2>/tmp/smoke2.err'
sleep 6; tmux send-keys -t s2 'echo boot-ok; hostname; echo \$SSH_CONNECTION' Enter; sleep 1.5; tmux capture-pane -p -t s2 | head -6
tmux send-keys -t s2 C-^ .; sleep 1.5; tail -2 /tmp/smoke2.err; tmux kill-session -t s2 2>/dev/null; true" </dev/null
$L ssh a 'pkill mosh-server; true' </dev/null

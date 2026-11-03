#!/bin/bash
# 256-colour screen dump for a big terminal (250x80): fg-only rows, then fg+bg rows.
clear
for r in $(seq 1 38); do
  for c in $(seq 1 62); do printf '\e[38;5;%dm%03d ' $((RANDOM%256)) $((RANDOM%1000)); done
  printf '\e[0m\n'
done
for r in $(seq 1 38); do
  for c in $(seq 1 62); do printf '\e[38;5;%d;48;5;%dm%03d ' $((RANDOM%256)) $((RANDOM%256)) $((RANDOM%1000)); done
  printf '\e[0m\n'
done

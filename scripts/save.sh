#!/usr/bin/env bash

echo "$(date) save $*" >>/tmp/tmux-sessions.log

/home/hyde/projects/tmux-sessions/target/debug/tmux-sessions save

#!/usr/bin/env bash

echo "$(date) restore $*" >>/tmp/tmux-sessions.log

/home/hyde/projects/tmux-sessions/target/debug/tmux-sessions restore

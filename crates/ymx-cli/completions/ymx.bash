#!/bin/bash
# ymx bash completion
# Source this file from your .bashrc or /etc/bash_completion.d/

_ymx() {
    local cur prev words cword
    _init_completion || return

    # All possible flags
    local flags="--entry --from-keyword --default-keyword --max-depth
                 --format --output --plain --plain-template --test
                 --help -h"

    # Format options
    local formats="json compact diagnostics"

    # Current word is a flag that expects a value
    if [[ "$prev" == "--entry" || "$prev" == "--from-keyword" ||
          "$prev" == "--default-keyword" || "$prev" == "--max-depth" ||
          "$prev" == "--format" || "$prev" == "--output" ]]; then
        return
    fi

    # Current word is --format with value needed
    if [[ "$prev" == "--format" ]]; then
        COMPREPLY=($(compgen -W "$formats" -- "$cur"))
        return
    fi

    # Complete flags
    if [[ "$cur" == -* ]]; then
        COMPREPLY=($(compgen -W "$flags" -- "$cur"))
        return
    fi

    # Complete file names
    _filedir '@(yml|yaml)'
}

complete -F _ymx ymx

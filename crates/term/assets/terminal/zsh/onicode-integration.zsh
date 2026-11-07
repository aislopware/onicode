# Onicode shell integration for zsh.
#
#   133;A / 133;B   prompt start / input start, inside PS1 so a theme that rebuilds it keeps them
#   133;C           output start (preexec)
#   133;D;<status>  command end (precmd)
#   OSC 7           working directory, percent-encoded (precmd)
#
# Sourced by the bootstrap .zshenv for interactive shells; safe to source twice.

[[ -n "${_onicode_integrated+set}" ]] && return 0
'builtin' 'typeset' -g _onicode_integrated=1
# 1 while a command runs: a C mark is open and needs its D.
'builtin' 'typeset' -gi _onicode_running=0
'builtin' 'typeset' -g _onicode_cwd_of= _onicode_cwd_path=

_onicode_mark() {
    'builtin' 'print' -nr -- $'\e]133;'"$1"$'\a'
}

# PWD as a URL path: everything but unreserved bytes and `/` percent-encoded. Redone only when
# the directory changes.
_onicode_cwd_url() {
    [[ "$PWD" == "$_onicode_cwd_of" ]] && 'builtin' 'return' 0
    'builtin' 'emulate' -L zsh
    'builtin' 'local' LC_ALL=C _onicode_c _onicode_h _onicode_out=
    'builtin' 'local' -i _onicode_i
    _onicode_cwd_of=$PWD
    if [[ "$PWD" != *[^A-Za-z0-9/._~-]* ]]; then
        _onicode_cwd_path=$PWD
        'builtin' 'return' 0
    fi
    for (( _onicode_i = 1; _onicode_i <= ${#PWD}; _onicode_i++ )); do
        _onicode_c=$PWD[_onicode_i]
        if [[ "$_onicode_c" == [A-Za-z0-9/._~-] ]]; then
            _onicode_out+=$_onicode_c
        else
            'builtin' 'printf' -v _onicode_h '%%%02X' "'$_onicode_c"
            _onicode_out+=$_onicode_h
        fi
    done
    _onicode_cwd_path=$_onicode_out
}

_onicode_precmd() {
    'builtin' 'local' -i _onicode_last=$?
    if (( _onicode_running )); then
        _onicode_mark "D;$_onicode_last"
        _onicode_running=0
    fi
    _onicode_cwd_url
    'builtin' 'print' -nr -- $'\e]7;file://'"${HOST}${_onicode_cwd_path}"$'\a'
    # %{ %} keeps the marks out of the prompt's width.
    if [[ "$PS1" != *$'\e]133;A'* ]]; then
        PS1=$'%{\e]133;A;redraw=1\a%}'"$PS1"$'%{\e]133;B\a%}'
    fi
    if [[ -n "$PS2" && "$PS2" != *$'\e]133;A;k=s'* ]]; then
        PS2=$'%{\e]133;A;k=s\a%}'"$PS2"$'%{\e]133;B\a%}'
    fi
    # Last of the precmd hooks, so a prompt theme cannot rebuild PS1 after us.
    if [[ "${precmd_functions[-1]}" != _onicode_precmd ]]; then
        precmd_functions=("${(@)precmd_functions:#_onicode_precmd}" _onicode_precmd)
    fi
}

_onicode_preexec() {
    _onicode_mark C
    _onicode_running=1
}

'builtin' 'typeset' -ga precmd_functions preexec_functions
precmd_functions+=(_onicode_precmd)
preexec_functions+=(_onicode_preexec)

# `sudo` keeps TERMINFO, or programs under it find no entry for TERM. sudoedit takes no
# --preserve-env and is left alone.
if [[ -n "${TERMINFO-}" ]] && (( ! $+functions[sudo] && ! $+aliases[sudo] )); then
    sudo() {
        'builtin' 'local' arg keep=--preserve-env=TERMINFO
        for arg in "$@"; do
            if [[ "$arg" == -e || "$arg" == --edit ]]; then
                keep=
                'builtin' 'break'
            fi
            [[ "$arg" == -* || "$arg" == *=* ]] || 'builtin' 'break'
        done
        'builtin' 'command' sudo ${keep:+"$keep"} "$@"
    }
fi

# `ssh` asks for xterm-256color: the far side has no xterm-ghostty entry.
if [[ "${TERM-}" == xterm-ghostty ]] && (( ! $+functions[ssh] && ! $+aliases[ssh] )); then
    ssh() {
        TERM=xterm-256color 'builtin' 'command' ssh "$@"
    }
fi

# Onicode shell integration for bash.
#
# Onicode starts `bash --posix` with ENV naming this file, the one startup file an interactive
# posix bash reads, so it runs where ~/.bash_profile or ~/.bashrc would. It leaves posix mode,
# restores the user's ENV (ONICODE_BASH_USER_ENV) and does the startup bash skipped: for a login
# shell (ONICODE_BASH_LOGIN) /etc/profile and the first of ~/.bash_profile, ~/.bash_login,
# ~/.profile; otherwise ~/.bashrc. Then it adds the marks:
#
#   133;A / 133;B   around PS1 (inside \[ \], so they take no width)
#   133;C           from a DEBUG trap, on the first command after a prompt
#   133;D;<status>  from PROMPT_COMMAND, when a C is open
#   OSC 7           working directory, percent-encoded, before each prompt
#
# Works on bash 3.2 and up. Onicode rewrites this file; edits are lost.

if [ -n "${ONICODE_BASH_INJECT-}" ]; then
    unset ENV
    if [ -n "${ONICODE_BASH_USER_ENV+set}" ]; then
        export ENV="$ONICODE_BASH_USER_ENV"
    fi
    set +o posix
    # Posix mode turns this on and leaving it does not turn it off (bash 4.4 and up).
    shopt -u inherit_errexit 2>/dev/null
    # Posix mode picks ~/.sh_history; keep one the environment named.
    case "$(declare -p HISTFILE 2>/dev/null)" in
        'declare -x'*) ;;
        *) HISTFILE="$HOME/.bash_history" ;;
    esac
    if [ -n "${ONICODE_BASH_LOGIN-}" ]; then
        [ -r /etc/profile ] && . /etc/profile
        for _onicode_rc in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
            if [ -r "$_onicode_rc" ]; then
                . "$_onicode_rc"
                break
            fi
        done
    elif [ -r "$HOME/.bashrc" ]; then
        . "$HOME/.bashrc"
    fi
    unset ONICODE_BASH_INJECT ONICODE_BASH_USER_ENV ONICODE_BASH_LOGIN _onicode_rc
fi

case $- in
    *i*) ;;
    *) return 0 ;;
esac
[ -n "${_onicode_integrated-}" ] && return 0
_onicode_integrated=1
# 1 while a command runs: a C mark is open and needs its D.
_onicode_running=0
# 1 once the prompt is up: the next command the DEBUG trap sees is the user's.
_onicode_armed=0

_onicode_mark() {
    printf '\033]133;%s\007' "$1"
}

# PWD as a URL path: everything but unreserved bytes and `/` percent-encoded. Redone only when
# the directory changes.
_onicode_cwd_url() {
    [ "$PWD" = "${_onicode_cwd_of-}" ] && return 0
    local LC_ALL=C _onicode_i=0 _onicode_c _onicode_h _onicode_out=
    _onicode_cwd_of=$PWD
    case $PWD in
        *[!A-Za-z0-9/._~-]*) ;;
        *)
            _onicode_cwd_path=$PWD
            return 0
            ;;
    esac
    while [ "$_onicode_i" -lt "${#PWD}" ]; do
        _onicode_c=${PWD:_onicode_i:1}
        case $_onicode_c in
            [A-Za-z0-9/._~-]) _onicode_out=$_onicode_out$_onicode_c ;;
            *)
                # bash 3.2 gives a byte past 0x7f as a negative number: keep its last two digits.
                printf -v _onicode_h '%02X' "'$_onicode_c"
                _onicode_out=$_onicode_out%${_onicode_h: -2}
                ;;
        esac
        _onicode_i=$((_onicode_i + 1))
    done
    _onicode_cwd_path=$_onicode_out
}

# First in PROMPT_COMMAND, so $? is still the command's status.
_onicode_precmd() {
    local _onicode_status=$?
    if [ "$_onicode_running" = 1 ]; then
        _onicode_mark "D;$_onicode_status"
        _onicode_running=0
    fi
    _onicode_armed=0
}

# Last in PROMPT_COMMAND: report the directory, rewrap PS1/PS2 (a theme may have rebuilt them)
# and arm the trap.
_onicode_arm() {
    _onicode_cwd_url
    printf '\033]7;file://%s%s\007' "$HOSTNAME" "$_onicode_cwd_path"
    local _onicode_b='\[\e]133;B\a\]'
    case $PS1 in
        *'133;A'*) ;;
        *) PS1='\[\e]133;A;redraw=last\a\]'"$PS1$_onicode_b" ;;
    esac
    case $PS2 in
        *'133;A;k=s'*) ;;
        *) PS2='\[\e]133;A;k=s\a\]'"$PS2$_onicode_b" ;;
    esac
    _onicode_armed=1
}

# Runs before every simple command, PROMPT_COMMAND's included; only the first one after a
# prompt is the user's.
_onicode_preexec() {
    [ "$_onicode_armed" = 1 ] || return 0
    case $BASH_COMMAND in
        _onicode_*) return 0 ;;
    esac
    _onicode_armed=0
    _onicode_running=1
    _onicode_mark C
}

# bash-preexec owns the DEBUG trap and calls these itself.
_onicode_bp_preexec() {
    _onicode_running=1
    _onicode_mark C
}
_onicode_bp_precmd() {
    _onicode_precmd
    _onicode_arm
}

if [ -n "${__bp_imported-}" ] || [ -n "${bash_preexec_imported-}" ]; then
    preexec_functions+=(_onicode_bp_preexec)
    precmd_functions+=(_onicode_bp_precmd)
elif [ -z "$(trap -p DEBUG)" ]; then
    trap '_onicode_preexec' DEBUG
    case "$(declare -p PROMPT_COMMAND 2>/dev/null)" in
        'declare -a'*) PROMPT_COMMAND=(_onicode_precmd "${PROMPT_COMMAND[@]}" _onicode_arm) ;;
        *) PROMPT_COMMAND="_onicode_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}; _onicode_arm" ;;
    esac
else
    # Someone else's DEBUG trap: prompt marks only, no command status.
    case "$(declare -p PROMPT_COMMAND 2>/dev/null)" in
        'declare -a'*) PROMPT_COMMAND=("${PROMPT_COMMAND[@]}" _onicode_arm) ;;
        *) PROMPT_COMMAND="${PROMPT_COMMAND:+$PROMPT_COMMAND; }_onicode_arm" ;;
    esac
fi

# `sudo` keeps TERMINFO, or programs under it find no entry for TERM. sudoedit takes no
# --preserve-env and is left alone.
if [ -n "${TERMINFO-}" ] && ! declare -F sudo >/dev/null && ! alias sudo >/dev/null 2>&1; then
    sudo() {
        local arg keep=--preserve-env=TERMINFO
        for arg in "$@"; do
            case $arg in
                -e | --edit)
                    keep=
                    break
                    ;;
                -* | *=*) ;;
                *) break ;;
            esac
        done
        command sudo ${keep:+"$keep"} "$@"
    }
fi

# `ssh` asks for xterm-256color: the far side has no xterm-ghostty entry.
if [ "${TERM-}" = xterm-ghostty ] && ! declare -F ssh >/dev/null && ! alias ssh >/dev/null 2>&1; then
    ssh() {
        TERM=xterm-256color command ssh "$@"
    }
fi

# Onicode shell integration for fish.
#
# Loaded because Onicode puts its directory first on XDG_DATA_DIRS, and fish reads
# <dir>/fish/vendor_conf.d/*.fish from each entry. The entry comes off again here, so programs
# the shell starts do not inherit it.
#
# fish 4 emits OSC 133 itself; fish 3 gets A/B around fish_prompt, C on fish_preexec and
# D;<status> on fish_postexec. Every fish gets OSC 7 before each prompt.
# Onicode rewrites this file; edits are lost.

if set -q ONICODE_SHELL_INTEGRATION_XDG_DIR
    set -l dirs (string split : -- "$XDG_DATA_DIRS")
    set -l keep (string match -v -- $ONICODE_SHELL_INTEGRATION_XDG_DIR $dirs)
    if test (count $keep) -gt 0
        set -gx XDG_DATA_DIRS (string join : -- $keep)
    else
        set -e XDG_DATA_DIRS
    end
    set -e ONICODE_SHELL_INTEGRATION_XDG_DIR
end

if status is-interactive; and not set -q __onicode_integrated
    set -g __onicode_integrated 1
    function __onicode_cwd --on-event fish_prompt
        printf '\e]7;file://%s%s\a' $hostname (string escape --style=url -- $PWD)
    end

    if not string match -rq '^[4-9]\.' -- $version
        function __onicode_mark
            printf '\e]133;%s\a' $argv[1]
        end

        # Once, at the first prompt: config.fish has run, so fish_prompt is the user's.
        function __onicode_wrap_prompt --on-event fish_prompt
            functions -e __onicode_wrap_prompt
            if functions -q fish_prompt
                functions -c fish_prompt __onicode_user_prompt
            else
                function __onicode_user_prompt
                    printf '> '
                end
            end
            function fish_prompt
                __onicode_mark 'A;redraw=1'
                __onicode_user_prompt
                __onicode_mark B
            end
        end

        function __onicode_preexec --on-event fish_preexec
            set -g __onicode_running 1
            __onicode_mark C
        end

        function __onicode_postexec --on-event fish_postexec
            set -l last $status
            if set -q __onicode_running
                set -e __onicode_running
                __onicode_mark "D;$last"
            end
        end
    end

    # `sudo` keeps TERMINFO, or programs under it find no entry for TERM. sudoedit takes no
    # --preserve-env and is left alone.
    if test -n "$TERMINFO"; and not functions -q sudo
        function sudo --wraps sudo
            set -l keep --preserve-env=TERMINFO
            for arg in $argv
                if test "$arg" = -e; or test "$arg" = --edit
                    set keep
                    break
                end
                string match -rq -- '^-|=' $arg; or break
            end
            command sudo $keep $argv
        end
    end

    # `ssh` asks for xterm-256color: the far side has no xterm-ghostty entry.
    if test "$TERM" = xterm-ghostty; and not functions -q ssh
        function ssh --wraps ssh
            TERM=xterm-256color command ssh $argv
        end
    end
end

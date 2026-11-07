# Onicode shell integration, zsh bootstrap.
#
# Onicode points ZDOTDIR here only so zsh reads this file first. It gives ZDOTDIR back
# (ONICODE_ZSH_ZDOTDIR holds the user's, if any), sources the user's .zshenv, and for an
# interactive shell loads onicode-integration.zsh. The user's .zprofile, .zshrc and .zlogin
# then load from their usual place. Onicode rewrites this file; edits are lost.

if [[ -n "${ONICODE_ZSH_ZDOTDIR+set}" ]]; then
    'builtin' 'export' ZDOTDIR="$ONICODE_ZSH_ZDOTDIR"
    'builtin' 'unset' 'ONICODE_ZSH_ZDOTDIR'
else
    'builtin' 'unset' 'ZDOTDIR'
fi

{
    'builtin' 'typeset' _onicode_rc="${ZDOTDIR-$HOME}/.zshenv"
    [[ ! -r "$_onicode_rc" ]] || 'builtin' 'source' '--' "$_onicode_rc"
} always {
    if [[ -o 'interactive' ]]; then
        'builtin' 'typeset' _onicode_rc="${${(%):-%x}:A:h}/onicode-integration.zsh"
        [[ ! -r "$_onicode_rc" ]] || 'builtin' 'source' '--' "$_onicode_rc"
    fi
    'builtin' 'unset' '_onicode_rc'
}

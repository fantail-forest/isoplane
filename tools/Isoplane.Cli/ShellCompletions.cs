namespace Isoplane.Cli;

internal enum CompletionShell { Bash, Zsh, Fish, PowerShell }

/// <summary>Shell adapters ask the argument parser for candidates without invoking a command.</summary>
internal static class ShellCompletions
{
    // All named shells are covered; invalid underlying enum values should still throw.
#pragma warning disable CS8524
    internal static string Script(CompletionShell shell) => shell switch
    {
        CompletionShell.Bash => """
            _isoplane_complete() {
                local candidate LC_ALL=C
                COMPREPLY=()
                while IFS= read -r candidate; do
                    COMPREPLY+=("$candidate")
                done < <(isoplane complete "${COMP_LINE:0:COMP_POINT}")
            }
            complete -o default -F _isoplane_complete isoplane
            """,
        CompletionShell.Zsh => """
            _isoplane_complete() {
                local -a candidates
                candidates=("${(@f)$(isoplane complete "$LBUFFER")}")
                if (( ${#candidates} )); then
                    compadd -- "${candidates[@]}"
                else
                    _files
                fi
            }
            compdef _isoplane_complete isoplane
            """,
        CompletionShell.Fish => """
            complete -c isoplane -a '(isoplane complete (commandline -cp))'
            """,
        CompletionShell.PowerShell => """
            Register-ArgumentCompleter -Native -CommandName isoplane -ScriptBlock {
                param($wordToComplete, $commandAst, $cursorPosition)
                $line = $commandAst.ToString()
                $length = $cursorPosition - $commandAst.Extent.StartOffset
                if ($length -lt $line.Length) { $line = $line.Substring(0, $length) }
                elseif ($length -gt $line.Length) { $line += ' ' }
                isoplane complete $line | ForEach-Object {
                    [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_)
                }
            }
            """,
    };

#pragma warning restore CS8524

    internal static IEnumerable<string> Candidates(string line) =>
        CliCommandLine.Create().Parse(line).GetCompletions().Select(item => item.Label);
}

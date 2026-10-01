# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=claude
# HERDR_INTEGRATION_VERSION=11

param([string]$Action = "")

if ($Action -ne "session" -and $Action -ne "reminder") { exit 0 }
if ($env:HERDR_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_PANE_ID)) { exit 0 }

# Repeats the awaiting-reply instruction on every prompt, since the SessionStart context is far
# back in a long session. Printed as is: it needs no hook input and no socket.
if ($Action -eq "reminder") {
    if ((Test-Path Env:CURSOR_VERSION) -or $env:HERDR_AWAITING_REPLY_INSTRUCTIONS -eq "0") { exit 0 }
    $context = "Herdr reminder: if you end this turn needing the user's answer or decision " +
        "before you can continue (a question, a choice, a confirmation, or a request to check " +
        "something first, even without a question mark), call the Bash tool with ``herdr agent " +
        "awaiting-reply`` (never write the command in your reply) on its own as the last command of the turn, right before your final message. Not " +
        "for AskUserQuestion or courtesy offers."
    @{ hookSpecificOutput = @{ hookEventName = "UserPromptSubmit"; additionalContext = $context } } |
        ConvertTo-Json -Compress -Depth 3 | Write-Output
    exit 0
}

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    exit 0
}

$propertyNames = @($payload.PSObject.Properties.Name)
if ((Test-Path Env:CURSOR_VERSION) -or $propertyNames -ccontains "cursor_version") { exit 0 }
if (-not ($propertyNames -ccontains "hook_event_name") -or $payload.hook_event_name -isnot [string] -or $payload.hook_event_name -cne "SessionStart") { exit 0 }
if (-not [string]::IsNullOrWhiteSpace($payload.agent_id)) { exit 0 }

# Ask the agent to report a turn that ends with a question, so herdr keeps its pane marked
# until the user answers. HERDR_AWAITING_REPLY_INSTRUCTIONS=0 turns the instruction off.
$contexts = @()
if ($env:HERDR_AWAITING_REPLY_INSTRUCTIONS -ne "0") {
    $context = "You run inside a Herdr pane. When you end a turn needing the user's answer " +
        "or decision before you can continue the work, run the shell command " +
        "``herdr agent awaiting-reply`` (call the Bash tool; never write the command in your " +
        "reply) on its own, as the last command of the turn, " +
        "right before your final message, so Herdr keeps your pane marked until the " +
        "user replies. This covers a plain-text question, a choice between options, a " +
        "confirmation before you proceed, and a request to check something before you " +
        "go on (`"let me know how it looks, then I will commit`"), even without a " +
        "question mark. Never append it to another command, never run it earlier in the " +
        "turn, and never run it for AskUserQuestion or any other question tool or " +
        "prompt answered inside the turn: Herdr already shows those as blocked. Run " +
        "it at most once per turn and ignore its failure. Do not run it when you " +
        "simply finished and ask nothing, or for courtesy offers such as asking " +
        "whether anything else is needed."
    $contexts += $context
}

if ($env:HERDR_AGENT_CONTEXT -ne "0") {
    $contexts += @'
[Herdr behavior context v1]
You are running in a Herdr pane. Herdr shows actual runtime activity, not promises.
- When authorized work has an executable next step, perform it instead of ending your turn with only a promise to continue.
- Only say work is running or queued when it has actually started and that claim is still accurate. This includes work started in an earlier turn.
- If you cannot start the next step, state what was not started and why. Ask explicitly when you need approval, a decision, credentials, or information.
- Report idle or finished truthfully. Do not fake activity, override an explicit stop, loop indefinitely, or start extra paid/model calls without authorization.
- User and repository instructions, approval requirements, and safety rules take precedence. This guidance is not permission to bypass them.
'@
}
if ($contexts.Count -gt 0) {
    @{ hookSpecificOutput = @{ hookEventName = "SessionStart"; additionalContext = ($contexts -join "`n`n") } } |
        ConvertTo-Json -Compress -Depth 3 | Write-Output
}

$sessionId = $payload.session_id
if ([string]::IsNullOrWhiteSpace($sessionId)) { exit 0 }

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
$herdr = if ([string]::IsNullOrWhiteSpace($env:HERDR_BIN_PATH)) { "herdr" } else { $env:HERDR_BIN_PATH }
try {
    $args = @(
        "pane",
        "report-agent-session",
        $env:HERDR_PANE_ID,
        "--source",
        "herdr:claude",
        "--agent",
        "claude",
        "--seq",
        "$seq",
        "--agent-session-id",
        "$sessionId"
    )
    if ($payload.transcript_path -is [string] -and -not [string]::IsNullOrWhiteSpace($payload.transcript_path)) {
        $args += @("--agent-session-path", "$($payload.transcript_path)")
    }
    if ($payload.hook_event_name -eq "SessionStart" -and $payload.source -is [string] -and -not [string]::IsNullOrWhiteSpace($payload.source)) {
        $args += @("--session-start-source", "$($payload.source)")
    }
    & $herdr @args 2>$null | Out-Null
} catch {
}

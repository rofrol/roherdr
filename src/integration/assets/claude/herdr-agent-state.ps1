# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=claude
# HERDR_INTEGRATION_VERSION=11

param([string]$Action = "")

if ($Action -ne "session") { exit 0 }
if ($env:HERDR_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_PANE_ID)) { exit 0 }

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
if ($env:HERDR_AWAITING_REPLY_INSTRUCTIONS -ne "0") {
    $context = "You run inside a Herdr pane. When you end a turn by asking the user something " +
        "they must answer or decide (a question, a choice between options, a " +
        "confirmation before you proceed), run the shell command " +
        "``herdr agent awaiting-reply`` right before your final message, so Herdr keeps " +
        "your pane marked until the user replies. Run it at most once per turn and " +
        "ignore its failure. Do not run it when you simply finished and ask nothing, " +
        "or for courtesy offers such as asking whether anything else is needed."
    @{ hookSpecificOutput = @{ hookEventName = "SessionStart"; additionalContext = $context } } |
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

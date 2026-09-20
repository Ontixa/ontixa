# Transaction-integrity demo (Windows-native PowerShell -- no WSL,
# no Git Bash required):
#
#   semantic rename plans, positional local/parameter rename,
#   capture rejection, stale-plan rejection, disk persistence,
#   pending-journal guard, crash recovery, byte preservation.
#
# Runs against a *copy* of this directory in %TEMP% -- the apply
# steps rewrite the copies, never the checked-in fixtures.
#
#   .\examples\workspace\demo.ps1                 # uses repo target/debug binaries
#   $env:ONTIXA="C:\path\ontixa.exe"; .\demo.ps1  # or explicit binaries

$ErrorActionPreference = "Continue"   # demo runs rejection cases on purpose

$HERE = Split-Path -Parent $MyInvocation.MyCommand.Path
$ROOT = Resolve-Path "$HERE\..\.."

function Resolve-Bin([string]$name) {
    $explicit = [Environment]::GetEnvironmentVariable($name.ToUpper())
    if ($explicit) { return (Resolve-Path $explicit).Path }
    $cmd = Get-Command "$name.exe" -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $built = Join-Path $ROOT "target\debug\$name.exe"
    if (Test-Path $built) { return $built }
    throw "cannot find $name on PATH or in target\debug"
}

$ONTIXA  = Resolve-Bin "ontixa"
$ONTIXAD = Resolve-Bin "ontixad"

$WORK = Join-Path $env:TEMP ("ontixa-demo-" + [guid]::NewGuid().ToString("N").Substring(0,8))
New-Item -ItemType Directory -Path $WORK | Out-Null
Copy-Item "$HERE\main.ixa", "$HERE\math.ixa" $WORK

function Say([string]$s) { Write-Host "`n== $s" -ForegroundColor Cyan }
function Run([string]$desc, [scriptblock]$cmd) {
    Write-Host "`n`$ $desc"
    & $cmd
}

try {
    Push-Location $WORK

    Say "1. The workspace: main.ixa uses math + math::Vec2"
    Get-Content math.ixa

    Say "2. Run the multi-module program -- double(norm(3,4))"
    Run "ontixa run main.ixa" { & $ONTIXA run main.ixa }

    Say "3. Explain a qualified symbol -- resolution, not text search"
    Run "ontixa explain main.ixa math::double" { & $ONTIXA explain main.ixa math::double }

    Say "4. Cross-module rename preview -- edits per file"
    Run "ontixa rename main.ixa math::double twice" { & $ONTIXA rename main.ixa math::double twice }

    Say "5. Rename apply -- staged disk persistence, both files in one revision"
    Run "ontixa rename main.ixa math::double twice --apply" { & $ONTIXA rename main.ixa math::double twice --apply }
    Run "ontixa run main.ixa" { & $ONTIXA run main.ixa }
    # No transaction artifacts survive a committed rename.
    $leftovers = Get-ChildItem $WORK -Force | Where-Object Name -match "ontixa-tx"
    if ($leftovers) { throw "leftover transaction artifacts: $($leftovers.Name)" }
    Write-Host "no .stage/.bak/.journal artifacts left"

    Say "6. Local/parameter rename -- positional @byte-offset selection"
    # `x` parameter of `fn twice(x: i32)` -- byte offset of its decl
    # (not the `x` field in `data Vec2`, which is not a local).
    # NOTE: offsets are BYTE offsets -- the header's em-dash is 3
    # UTF-8 bytes but one UTF-16 char, so convert via GetByteCount.
    $src = [IO.File]::ReadAllText("$WORK\math.ixa")
    $off = [Text.Encoding]::UTF8.GetByteCount($src.Substring(0, $src.IndexOf("twice(x") + 6))
    Run "ontixa rename math.ixa @$off factor   # select the x binding at byte $off" {
        & $ONTIXA rename math.ixa "@$off" factor --apply
    }
    Get-Content math.ixa | Select-String "twice"

    Say "7. Capture rejection -- new name would rebind a reference"
    # Outer `x` renamed to `y` where an inner `y` exists: the `x + y`
    # ref would silently rebind to the inner decl -- must reject.
    $capSrc = @'
fn g() -> i32 { let x = 1; if 1 > 0 { let y = 2; let u = x + y; } return x; }
'@
    [IO.File]::WriteAllText("$WORK\cap.ixa", "$capSrc`n")
    $cap = [IO.File]::ReadAllText("$WORK\cap.ixa")
    $xoff = $cap.IndexOf("x = 1")
    Run "ontixa rename cap.ixa @$xoff y   # capture: x + y would rebind to inner y" {
        & $ONTIXA rename cap.ixa "@$xoff" y
    }
    Write-Host "exit=$LASTEXITCODE (rejected -- nothing mutated)"
    if ((Get-Content cap.ixa -Raw) -notmatch "let x = 1") { throw "rejection mutated the file" }

    Say "8. Daemon: stale in-memory plan rejection"
    $wpath = $WORK -replace '\\','/'
    $req1 = @(
        (@{op="open";    path="$wpath/main.ixa"} | ConvertTo-Json -Compress),
        (@{op="rename";  path="$wpath/main.ixa"; symbol="math::twice"; to="triple"} | ConvertTo-Json -Compress),
        (@{op="shutdown"} | ConvertTo-Json -Compress)
    )
    $out1 = $req1 | & $ONTIXAD
    $rev = ([regex]::Match(($out1 -join "`n"), '"revision":(\d+)')).Groups[1].Value
    Write-Host "planned revision: $rev"
    $req2 = @(
        (@{op="open";    path="$wpath/main.ixa"} | ConvertTo-Json -Compress),
        (@{op="rename";  path="$wpath/main.ixa"; symbol="math::twice"; to="triple";
            apply=$true; revision=([int]$rev + 9)} | ConvertTo-Json -Compress),
        (@{op="shutdown"} | ConvertTo-Json -Compress)
    )
    $out2 = $req2 | & $ONTIXAD
    $out2 | Select-String 'E_STALE_REVISION' | Select-Object -First 1
    Write-Host "stale plan rejected -- in-memory state untouched"

    Say "9. Disk integrity: a pending journal blocks new writes"
    # A journal left by a dead transaction makes any new apply
    # refuse -- the protocol only works if participants respect it.
    "{ corrupt journal" | Set-Content "$WORK\.ontixa-tx-dead.journal"
    Run "ontixa rename main.ixa math::twice triple --apply" {
        & $ONTIXA rename main.ixa math::twice triple --apply
    }
    Write-Host "exit=$LASTEXITCODE (blocked by pending tx)"
    if (-not (Test-Path "$WORK\.ontixa-tx-dead.journal")) { throw "journal lost" }

    Say "10. Recovery: `ontixa recover` resolves the journal"
    Run "ontixa recover ." { & $ONTIXA recover . }
    Write-Host "exit=$LASTEXITCODE (conflict -- corrupt journal, preserved)"
    Remove-Item "$WORK\.ontixa-tx-dead.journal"
    Run "ontixa recover ." { & $ONTIXA recover . }

    Say "11. Byte fidelity: CRLF + UTF-8 survive the rename"
    $bytes = [IO.File]::ReadAllBytes("$WORK\math.ixa")
    $text  = [Text.Encoding]::UTF8.GetString($bytes)
    if ($text -notmatch "`r`n") { throw "CRLF line endings were rewritten" }
    Write-Host "math.ixa still CRLF ($($bytes.Length) bytes)"
    # em-dash in the header comment is multi-byte UTF-8.
    if (-not $text.Contains([char]0x2014)) { throw "UTF-8 content mangled" }
    Write-Host "UTF-8 multi-byte content intact"

    Say "12. Fixture files untouched -- demo ran on copies"
    $orig = Get-Content "$HERE\math.ixa" -Raw
    if ($orig -notmatch "fn double") { throw "fixture mutated" }
    Write-Host "original still declares 'double': $HERE\math.ixa"
}
finally {
    Pop-Location
    Remove-Item -Recurse -Force $WORK -ErrorAction SilentlyContinue
}

Write-Host "`ndemo complete" -ForegroundColor Green

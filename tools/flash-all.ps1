<#
.SYNOPSIS
    Builds and flashes all three boards, checking after each one that it
    actually came up.

.DESCRIPTION
    Walks the whole chain in order - wand, bridge, controller - and after each
    flash listens to that board to confirm it is running, rather than trusting
    that "flashing completed" meant it worked. Finishes with an end-to-end test
    that casts a spell from the wand without anyone waving anything, and checks
    it arrives on the STM32.

    Ports are detected automatically where possible; pass -WandPort / -BridgePort
    to override.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\flash-all.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\flash-all.ps1 -WandPort COM6 -BridgePort COM9
#>
[CmdletBinding()]
param(
    [string]$WandPort,
    [string]$BridgePort,
    # Run without pausing between steps.
    [switch]$NonInteractive,
    # Skip the end-to-end selftest at the end.
    [switch]$SkipEndToEnd,
    # Only check the toolchain and list the boards, flash nothing.
    [switch]$CheckOnly
)

$ErrorActionPreference = 'Continue'
$RepoRoot = Split-Path -Parent $PSScriptRoot
$LogDir   = Join-Path $RepoRoot 'tools\logs'
New-Item -ItemType Directory -Force -Path $LogDir | Out-Null

$script:Results = New-Object System.Collections.ArrayList

# --- output helpers --------------------------------------------------------

function Write-Step  ($m) { Write-Host ""; Write-Host "==> $m" -ForegroundColor Cyan }
function Write-Ok    ($m) { Write-Host "    [ok]   $m" -ForegroundColor Green }
function Write-Note  ($m) { Write-Host "    ...    $m" -ForegroundColor DarkGray }
function Write-Warn2 ($m) { Write-Host "    [warn] $m" -ForegroundColor Yellow }
function Write-Fail  ($m) { Write-Host "    [FAIL] $m" -ForegroundColor Red }

function Add-Result ($Stage, $State, $Detail) {
    [void]$script:Results.Add([pscustomobject]@{ Stage = $Stage; State = $State; Detail = $Detail })
}

function Test-Command ($Name) { $null -ne (Get-Command $Name -ErrorAction SilentlyContinue) }

function Wait-Continue ($Message) {
    if ($NonInteractive) { return }
    Write-Host ""
    Read-Host "    $Message - press Enter to continue" | Out-Null
}

# Runs a command, captures both streams, and kills it after a timeout. Used for
# the monitors, which stream forever and have to be stopped rather than waited
# for.
function Invoke-Timed ($File, $ArgList, $WorkDir, $TimeoutSec) {
    $outFile = [IO.Path]::GetTempFileName()
    $errFile = [IO.Path]::GetTempFileName()
    $timedOut = $false
    $exitCode = -1
    try {
        $p = Start-Process -FilePath $File -ArgumentList $ArgList -WorkingDirectory $WorkDir `
                -NoNewWindow -PassThru -RedirectStandardOutput $outFile -RedirectStandardError $errFile
        if (-not $p.WaitForExit($TimeoutSec * 1000)) {
            $timedOut = $true
            try { $p.Kill() } catch { }
            $p.WaitForExit()
        }
        $exitCode = $p.ExitCode
    } catch {
        return [pscustomobject]@{ Text = $_.Exception.Message; ExitCode = -1; TimedOut = $false }
    }
    $text = ''
    if (Test-Path $outFile) { $text += (Get-Content $outFile -Raw) }
    if (Test-Path $errFile) { $text += (Get-Content $errFile -Raw) }
    Remove-Item $outFile, $errFile -Force -ErrorAction SilentlyContinue
    [pscustomobject]@{ Text = $text; ExitCode = $exitCode; TimedOut = $timedOut }
}

# Opens a COM port and echoes whatever the board says for a few seconds.
#
# -PulseReset is for the NodeMCU: its USB adapter's DTR and RTS lines are wired
# to GPIO0 and RESET, so opening the port with the defaults can hold the board
# in reset or drop it into flash mode. Releasing DTR and pulsing RTS boots it
# into run mode and gets us the banner.
function Read-Serial ($Port, $Baud, $Seconds, [switch]$PulseReset) {
    $sp = New-Object System.IO.Ports.SerialPort $Port, $Baud, 'None', 8, 'One'
    $sp.ReadTimeout = 500
    $sp.DtrEnable = $false
    $sp.RtsEnable = $false
    try {
        $sp.Open()
    } catch {
        Write-Warn2 "could not open $Port : $($_.Exception.Message)"
        return ''
    }
    if ($PulseReset) {
        $sp.RtsEnable = $true
        Start-Sleep -Milliseconds 100
        $sp.RtsEnable = $false
        Start-Sleep -Milliseconds 300
        $sp.DiscardInBuffer()
    }
    $sb = New-Object System.Text.StringBuilder
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        try {
            $chunk = $sp.ReadExisting()
            if ($chunk) {
                [void]$sb.Append($chunk)
                Write-Host $chunk -NoNewline -ForegroundColor DarkGray
            }
        } catch { }
        Start-Sleep -Milliseconds 100
    }
    Write-Host ""
    $sp.Close()
    $sp.Dispose()
    $sb.ToString()
}

function Get-ComPorts {
    Get-CimInstance Win32_PnPEntity -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '\(COM\d+\)' } |
        ForEach-Object {
            $null = $_.Name -match '\((COM\d+)\)'
            [pscustomobject]@{ Port = $matches[1]; Name = $_.Name }
        } | Sort-Object { [int]($_.Port -replace 'COM', '') }
}

# Picks a port by matching the USB adapter's description, then lets the user
# confirm or correct it. The wand appears as a plain CDC device because the
# ESP32-S3 has native USB; the NodeMCU shows up as its CH340 or CP210x adapter.
function Select-Port ($Label, $Pattern, $Preset, $Ports) {
    if ($Preset) { return $Preset }
    $guess = $Ports | Where-Object { $_.Name -match $Pattern } | Select-Object -First 1
    if ($NonInteractive) {
        if ($guess) {
            Write-Ok "$Label -> $($guess.Port)  ($($guess.Name))"
            return $guess.Port
        }
        Write-Warn2 "no port matched the $Label - pass it explicitly"
        return ''
    }
    Write-Host ""
    Write-Host "    Serial ports:" -ForegroundColor DarkGray
    foreach ($p in $Ports) { Write-Host "      $($p.Port)  $($p.Name)" -ForegroundColor DarkGray }
    $default = if ($guess) { $guess.Port } else { '' }
    $answer = Read-Host "    Which port is the $Label`? [$default]"
    if ([string]::IsNullOrWhiteSpace($answer)) { return $default }
    $answer.Trim().ToUpper()
}

# --- 0. toolchain check ----------------------------------------------------

Write-Host ""
Write-Host "Motion Spells - build, flash and verify" -ForegroundColor White
Write-Host "repository: $RepoRoot"

Write-Step "Toolchain"
# A missing tool only takes out the stage that needs it, so a partial setup can
# still flash the boards it is able to.
$have = @{ }
foreach ($tool in @('cargo', 'espflash', 'probe-rs', 'arduino-cli')) {
    $have[$tool] = Test-Command $tool
    if ($have[$tool]) { Write-Ok "$tool found" } else { Write-Warn2 "$tool missing" }
}
if (-not $have['cargo']) {
    Write-Host ""
    Write-Host "cargo is missing - run tools\install.ps1 first, then open a new terminal." -ForegroundColor Red
    exit 1
}
if (-not (Test-Path (Join-Path $RepoRoot 'wand\model\spells.tflite'))) {
    Write-Fail "wand\model\spells.tflite is missing - the wand firmware cannot be built without it"
    exit 1
}
$missing = @($have.Keys | Where-Object { -not $have[$_] })
if ($missing.Count -eq 0) {
    Add-Result 'toolchain' 'ok' 'cargo, espflash, probe-rs, arduino-cli'
} else {
    Add-Result 'toolchain' 'partial' ("missing: " + ($missing -join ', '))
}

# --- 1. boards -------------------------------------------------------------

Write-Step "Boards"
$ports = @(Get-ComPorts)
if ($ports.Count -eq 0) {
    Write-Warn2 "no COM ports at all - are the boards plugged in?"
}

$haveProbe = $false
$probeId = $null
if ($have['probe-rs']) {
    $probes = Invoke-Timed 'probe-rs' @('list') $RepoRoot 20
    Write-Host ($probes.Text.Trim()) -ForegroundColor DarkGray

    # The wand's own USB shows up as an "ESP JTAG" debug probe, so with all
    # three boards connected probe-rs sees two probes and stops to ask which one
    # to use - which, with no console to answer on, just fails. Name the ST-Link
    # explicitly instead. Its identifier is the VID:PID:serial in the listing.
    foreach ($line in ($probes.Text -split "`n")) {
        if ($line -match 'ST-?LINK' -and $line -match '--\s*([0-9a-fA-F]{4}:[0-9a-fA-F]{4}:\S+?)\s*\(') {
            $probeId = $matches[1]
            break
        }
    }

    if ($probeId) {
        Write-Ok "ST-Link selected: $probeId"
        $haveProbe = $true
    } else {
        Write-Warn2 "no ST-Link found - the STM32 steps will be skipped"
    }
}

$WandPort   = Select-Port 'wand (ESP32-S3)'      'USB Serial Device|JTAG|CDC|USB-Enhanced|Silicon Labs|CH34' $WandPort   $ports
$BridgePort = Select-Port 'bridge (NodeMCU)'     'CH34|CP210|Silicon Labs|USB-SERIAL'                        $BridgePort $ports

Write-Host ""
Write-Host "    wand:   $WandPort" -ForegroundColor White
Write-Host "    bridge: $BridgePort" -ForegroundColor White
Write-Host "    stm32:  $(if ($haveProbe) { 'ST-Link' } else { 'not detected' })" -ForegroundColor White

if ($WandPort -and $WandPort -eq $BridgePort) {
    Write-Fail "the wand and the bridge cannot be the same port"
    exit 1
}

if ($CheckOnly) {
    Write-Host ""
    Write-Host "Check only - nothing flashed." -ForegroundColor White
    exit 0
}

# --- 2. wand ---------------------------------------------------------------

$wandDir = Join-Path $RepoRoot 'wand'
$wandElf = Join-Path $wandDir 'target\xtensa-esp32s3-none-elf\release\spells'

Write-Step "1/3  Wand (ESP32-S3): gesture recognition + ESP-NOW"
Wait-Continue "Connect the wand by USB"

if (-not $have['espflash']) {
    Write-Warn2 "espflash missing - skipping"
    Add-Result 'wand' 'skipped' 'espflash not installed'
} elseif (-not $WandPort) {
    Write-Fail "no port chosen for the wand"
    Add-Result 'wand' 'FAILED' 'no port'
} else {
    Write-Note "cargo build --release --bin spells --features ml,radio"
    $build = Invoke-Timed 'cargo' @('build', '--release', '--bin', 'spells', '--features', 'ml,radio') $wandDir 1800
    Set-Content -Path (Join-Path $LogDir 'wand-build.log') -Value $build.Text
    if ($build.ExitCode -ne 0) {
        Write-Fail "build failed - see tools\logs\wand-build.log"
        Write-Host ($build.Text | Select-Object -Last 40) -ForegroundColor DarkGray
        Add-Result 'wand' 'FAILED' 'build failed'
    } else {
        Write-Ok "built"
        Write-Note "espflash flash --port $WandPort"
        $flash = Invoke-Timed 'espflash' @('flash', '--port', $WandPort, $wandElf) $wandDir 300
        Set-Content -Path (Join-Path $LogDir 'wand-flash.log') -Value $flash.Text
        if ($flash.ExitCode -ne 0) {
            Write-Fail "flashing failed - see tools\logs\wand-flash.log"
            Write-Host ($flash.Text | Select-Object -Last 30) -ForegroundColor DarkGray
            Add-Result 'wand' 'FAILED' 'flash failed'
        } else {
            Write-Ok "flashed"
            Write-Note "listening on $WandPort for 12s - it should announce itself"
            Start-Sleep -Seconds 2
            $out = Read-Serial $WandPort 115200 12
            Set-Content -Path (Join-Path $LogDir 'wand-boot.log') -Value $out

            $sawBanner = $out -match 'Motion Spells'
            $sawRadio  = $out -match 'Radio up'
            $sawSensor = $out -match 'Sensor OK'

            if ($sawBanner -and $sawRadio -and $sawSensor) {
                Write-Ok "wand running: model loaded, radio up, MPU6050 answering"
                Add-Result 'wand' 'ok' 'banner + radio + sensor'
            } elseif ($sawBanner) {
                $lacking = @()
                if (-not $sawRadio)  { $lacking += 'radio did not start' }
                if (-not $sawSensor) { $lacking += 'no answer from the MPU6050 (check SDA=GPIO12, SCL=GPIO13)' }
                Write-Warn2 ("wand booted but: " + ($lacking -join '; '))
                Add-Result 'wand' 'partial' ($lacking -join '; ')
            } else {
                # espflash resets the chip on exit, so silence here usually means
                # the console output is going somewhere else, not that it hung.
                Write-Warn2 "flashed, but nothing was read back on $WandPort"
                Write-Warn2 "check with: espflash monitor --port $WandPort"
                Add-Result 'wand' 'partial' 'flashed, no console output captured'
            }
        }
    }
}

# --- 3. bridge -------------------------------------------------------------

$bridgeDir    = Join-Path $RepoRoot 'controller'
$bridgeSketch = 'nodemcu-bridge'
$fqbn         = 'esp8266:esp8266:nodemcuv2'

Write-Step "2/3  Bridge (NodeMCU ESP8266): ESP-NOW to UART"
Wait-Continue "Connect the NodeMCU by USB"

if (-not $have['arduino-cli']) {
    Write-Warn2 "arduino-cli missing - skipping. The bridge only needs reflashing if its"
    Write-Warn2 "sketch changed, so this is not usually a problem."
    Add-Result 'bridge' 'skipped' 'arduino-cli not installed'
} elseif (-not $BridgePort) {
    Write-Fail "no port chosen for the bridge"
    Add-Result 'bridge' 'FAILED' 'no port'
} else {
    Write-Note "arduino-cli compile --fqbn $fqbn $bridgeSketch"
    $compile = Invoke-Timed 'arduino-cli' @('compile', '--fqbn', $fqbn, $bridgeSketch) $bridgeDir 600
    Set-Content -Path (Join-Path $LogDir 'bridge-build.log') -Value $compile.Text
    if ($compile.ExitCode -ne 0) {
        Write-Fail "compile failed - see tools\logs\bridge-build.log"
        Write-Host ($compile.Text | Select-Object -Last 30) -ForegroundColor DarkGray
        Add-Result 'bridge' 'FAILED' 'compile failed'
    } else {
        Write-Ok "compiled"
        Write-Note "arduino-cli upload -p $BridgePort"
        $upload = Invoke-Timed 'arduino-cli' @('upload', '-p', $BridgePort, '--fqbn', $fqbn, $bridgeSketch) $bridgeDir 300
        Set-Content -Path (Join-Path $LogDir 'bridge-flash.log') -Value $upload.Text
        if ($upload.ExitCode -ne 0) {
            Write-Fail "upload failed - see tools\logs\bridge-flash.log"
            Write-Host ($upload.Text | Select-Object -Last 30) -ForegroundColor DarkGray
            Write-Warn2 "if this says the port is busy, close any serial monitor and try again"
            Add-Result 'bridge' 'FAILED' 'upload failed'
        } else {
            Write-Ok "uploaded"
            Write-Note "resetting into run mode and listening for 10s"
            $out = Read-Serial $BridgePort 115200 10 -PulseReset
            Set-Content -Path (Join-Path $LogDir 'bridge-boot.log') -Value $out

            # Unreadable characters before the banner are the ESP8266 bootloader,
            # which prints at 74880 baud whatever the sketch selects afterwards.
            if ($out -match 'esp-now bridge' -or $out -match 'listening for spells') {
                Write-Ok "bridge running and listening on channel 1"
                Add-Result 'bridge' 'ok' 'banner seen'
                if ($out -match 'mac:\s*([0-9A-Fa-f:]+)') { Write-Note "bridge MAC $($matches[1])" }
            } else {
                Write-Warn2 "uploaded, but no banner - it may just have missed the reset window"
                Add-Result 'bridge' 'partial' 'uploaded, banner not captured'
            }
        }
    }
}

# --- 4. controller ---------------------------------------------------------

$ctrlDir = Join-Path $RepoRoot 'controller'
$ctrlElf = Join-Path $ctrlDir 'target\thumbv8m.main-none-eabihf\release\controller'

Write-Step "3/3  Controller (STM32U545RE-Q): OLED"
Wait-Continue "Connect the Nucleo by USB"

if (-not $haveProbe) {
    Write-Warn2 "no ST-Link - skipping"
    Add-Result 'controller' 'skipped' 'no probe detected'
} else {
    Write-Note "cargo build --release"
    $build = Invoke-Timed 'cargo' @('build', '--release') $ctrlDir 900
    Set-Content -Path (Join-Path $LogDir 'controller-build.log') -Value $build.Text
    if ($build.ExitCode -ne 0) {
        Write-Fail "build failed - see tools\logs\controller-build.log"
        Write-Host ($build.Text | Select-Object -Last 40) -ForegroundColor DarkGray
        Add-Result 'controller' 'FAILED' 'build failed'
    } else {
        Write-Ok "built"
        Write-Note "probe-rs run --chip STM32U545RETx (flashes, then streams logs for 20s)"
        $run = Invoke-Timed 'probe-rs' @('run', '--chip', 'STM32U545RETx', '--probe', $probeId, $ctrlElf) $ctrlDir 20
        Set-Content -Path (Join-Path $LogDir 'controller-boot.log') -Value $run.Text
        Write-Host ($run.Text.Trim()) -ForegroundColor DarkGray

        $sawDisplay = $run.Text -match 'display responded at'
        $sawWaiting = $run.Text -match 'waiting for spells'

        if ($sawDisplay -and $sawWaiting) {
            Write-Ok "controller running, OLED answering, listening on LPUART1"
            Add-Result 'controller' 'ok' 'display + uart ready'
        } elseif ($run.Text -match 'no display at') {
            Write-Fail "no OLED on the bus - check SCL=D15 (PB6), SDA=D14 (PB7), 3V3, GND"
            Add-Result 'controller' 'FAILED' 'OLED not responding'
        } elseif ($run.Text -match 'Error|error:') {
            Write-Fail "probe-rs failed - see tools\logs\controller-boot.log"
            Add-Result 'controller' 'FAILED' 'probe-rs error'
        } else {
            Write-Warn2 "flashed, but the expected log lines did not appear"
            Add-Result 'controller' 'partial' 'flashed, logs unclear'
        }
    }
}

# --- 5. end to end ---------------------------------------------------------
# The one link nothing above proves is the wire from the bridge to the STM32.
# Flashing the wand with the selftest feature makes it cast four spells at boot
# with no gesture, so the whole chain can be checked in one go.

if ($SkipEndToEnd) {
    Add-Result 'end-to-end' 'skipped' '-SkipEndToEnd'
} elseif (-not $haveProbe -or -not $WandPort -or -not $have['espflash']) {
    Write-Step "End-to-end test"
    Write-Warn2 "needs both the wand and the ST-Link - skipping"
    Add-Result 'end-to-end' 'skipped' 'wand or probe missing'
} else {
    Write-Step "End-to-end: wand -> ESP-NOW -> bridge -> UART -> OLED"
    Write-Host "    The wand will be flashed with the selftest build, which casts" -ForegroundColor DarkGray
    Write-Host "    LEFT, RIGHT, UP and DOWN at boot. Watch the OLED - each should" -ForegroundColor DarkGray
    Write-Host "    appear on it. The normal firmware is put back afterwards." -ForegroundColor DarkGray
    Write-Host ""
    Write-Host "    Check the bridge is wired to the Nucleo: D4 (GPIO2) -> D0 (PA3), GND -> GND." -ForegroundColor Yellow
    Wait-Continue "Ready"

    $e2eLog = Join-Path $LogDir 'end-to-end.log'
    Remove-Item $e2eLog -Force -ErrorAction SilentlyContinue

    Write-Note "building the selftest firmware"
    $build = Invoke-Timed 'cargo' @('build', '--release', '--bin', 'spells', '--features', 'ml,radio,selftest') $wandDir 1800
    if ($build.ExitCode -ne 0) {
        Write-Fail "selftest build failed"
        Add-Result 'end-to-end' 'FAILED' 'build failed'
    } else {
        # Start the STM32 log first, so nothing is missed. Only one process can
        # hold the ST-Link at a time, so this has to be stopped before any other
        # probe-rs command runs.
        Write-Note "attaching to the STM32"
        $errLog = "$e2eLog.err"
        $monitor = Start-Process -FilePath 'probe-rs' `
            -ArgumentList @('attach', '--chip', 'STM32U545RETx', '--probe', $probeId, $ctrlElf) `
            -WorkingDirectory $ctrlDir -NoNewWindow -PassThru `
            -RedirectStandardOutput $e2eLog -RedirectStandardError $errLog
        Start-Sleep -Seconds 3

        try {
            Write-Note "flashing the wand with the selftest build"
            $flash = Invoke-Timed 'espflash' @('flash', '--port', $WandPort, $wandElf) $wandDir 300
            if ($flash.ExitCode -ne 0) {
                Write-Fail "flashing the selftest build failed"
                Add-Result 'end-to-end' 'FAILED' 'flash failed'
            } else {
                Write-Note "waiting 25s while the wand casts four spells - watch the OLED"
                Start-Sleep -Seconds 25
            }
        } finally {
            try { $monitor.Kill() } catch { }
            Start-Sleep -Milliseconds 500
        }

        $log = ''
        if (Test-Path $e2eLog) { $log = Get-Content $e2eLog -Raw }
        if (Test-Path $errLog) { $log += (Get-Content $errLog -Raw); Remove-Item $errLog -Force -ErrorAction SilentlyContinue }
        Set-Content -Path $e2eLog -Value $log

        Write-Host ""
        Write-Host "    STM32 log:" -ForegroundColor DarkGray
        Write-Host ($log.Trim()) -ForegroundColor DarkGray

        if ($log -match 'cast:\s*(\w+)') {
            Write-Ok "spells arrived on the STM32 - the whole chain works"
            Add-Result 'end-to-end' 'ok' 'spell received and displayed'
        } elseif ($log -match 'unknown spell|line too long') {
            Write-Warn2 "bytes are arriving but do not parse - the link works, the content does not"
            Add-Result 'end-to-end' 'partial' 'garbled data, check the baud rate'
        } elseif ($log -match 'uart read error') {
            Write-Fail "framing errors on LPUART1 - both ends must be at 115200"
            Add-Result 'end-to-end' 'FAILED' 'uart framing errors'
        } else {
            Write-Fail "nothing reached the STM32"
            Write-Host ""
            Write-Host "    Work backwards from the display:" -ForegroundColor Yellow
            Write-Host "      1. the wire:  NodeMCU D4 (GPIO2) -> Nucleo D0 (PA3), and a shared GND" -ForegroundColor Yellow
            Write-Host "      2. the radio: watch the bridge's console while the wand boots -" -ForegroundColor Yellow
            Write-Host "                    arduino-cli monitor -p $BridgePort -c baudrate=115200" -ForegroundColor Yellow
            Write-Host "                    lines like 'A0:F2:.. -> UP' mean ESP-NOW is fine and" -ForegroundColor Yellow
            Write-Host "                    the fault is between the bridge and the STM32" -ForegroundColor Yellow
            Write-Host "      3. power:     the NodeMCU needs its own USB or the Nucleo's 5V pin," -ForegroundColor Yellow
            Write-Host "                    not 3V3 - transmit spikes brown out that regulator" -ForegroundColor Yellow
            Add-Result 'end-to-end' 'FAILED' 'no spells reached the STM32'
        }
    }

    # Leave the wand on the normal firmware; the selftest build casts spells on
    # every boot, which is not what you want during a demonstration.
    Write-Note "restoring the normal wand firmware"
    $restore = Invoke-Timed 'cargo' @('build', '--release', '--bin', 'spells', '--features', 'ml,radio') $wandDir 1800
    if ($restore.ExitCode -eq 0) {
        $flashBack = Invoke-Timed 'espflash' @('flash', '--port', $WandPort, $wandElf) $wandDir 300
        if ($flashBack.ExitCode -eq 0) {
            Write-Ok "wand back on the normal firmware"
        } else {
            Write-Warn2 "could not reflash - the wand is still on the selftest build"
            Write-Warn2 "fix with: cd wand; cargo build --release --bin spells --features ml,radio; espflash flash --port $WandPort $wandElf"
        }
    }
}

# --- summary ---------------------------------------------------------------

Write-Host ""
Write-Host "--------------------------------------------------------------" -ForegroundColor White
Write-Host " Results" -ForegroundColor White
Write-Host "--------------------------------------------------------------" -ForegroundColor White
foreach ($r in $script:Results) {
    $colour = switch ($r.State) {
        'ok'      { 'Green' }
        'skipped' { 'DarkGray' }
        'partial' { 'Yellow' }
        default   { 'Red' }
    }
    Write-Host ("  {0,-12} {1,-8} {2}" -f $r.Stage, $r.State, $r.Detail) -ForegroundColor $colour
}
Write-Host ""
Write-Host "Logs: tools\logs" -ForegroundColor DarkGray
Write-Host ""

$failed = @($script:Results | Where-Object { $_.State -eq 'FAILED' })
if ($failed.Count -eq 0) {
    Write-Host "All boards flashed and answering. To run the router side:" -ForegroundColor Green
    Write-Host ""
    Write-Host "    cd controller\manager" -ForegroundColor Cyan
    Write-Host "    python manager.py" -ForegroundColor Cyan
    Write-Host ""
    Write-Host "It starts in dry run and prints the commands it would send." -ForegroundColor DarkGray
} else {
    Write-Host "Some stages failed - see above and the logs." -ForegroundColor Red
    exit 1
}
